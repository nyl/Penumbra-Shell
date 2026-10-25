//! Progressive Blur Backdrop Pipeline for Android 17 QuickSettings Drawer.
//!
//! # Architecture & Pipeline
//! When the user initiates a drag-down gesture from the top bar or homescreen, the underlying
//! scene (wallpaper, active application, or overview tasks) is captured into `scene_fbo`.
//! A 2-pass Dual-Kawase blur is executed on an offscreen framebuffer downscaled to 1/4 resolution,
//! providing a completely diffused ambient field.
//!
//! A progressive cross-fade shader (`quicksettings_backdrop.frag`) blends the sharp scene texture
//! with the diffused blurred texture using `smoothstep(0.05, 0.95, u_progress)` and overlays an
//! organic dark scrim (`#0b0a0e`, alpha scaled as `u_progress * 0.40`).

use std::ffi::CString;
use tracing::{debug, error, info};

use smithay::backend::{
    allocator::Fourcc,
    renderer::{
        gles::{ffi, GlesError, GlesRenderer, GlesTexture},
        Offscreen, Texture,
    },
};

const VERTEX_SHADER_SRC: &str = include_str!("quad.vert");
const KAWASE_DOWN_SRC: &str = include_str!("kawase_down.frag");
const KAWASE_BLUR_SRC: &str = include_str!("kawase_blur.frag");
const BACKDROP_SRC: &str = include_str!("quicksettings_backdrop.frag");

/// Downscale factor for the Kawase blur pass (1/4 resolution).
const DOWNSCALE_FACTOR: i32 = 4;

/// Pipeline for offscreen scene capture, Dual-Kawase blur, and progressive cross-fade rendering.
pub struct QuickSettingsBackdropPipeline {
    width: i32,
    height: i32,
    down_w: i32,
    down_h: i32,

    scene_tex: Option<GlesTexture>,

    ping_fbo: ffi::types::GLuint,
    ping_tex: ffi::types::GLuint,
    blurred_fbo: ffi::types::GLuint,
    blurred_tex: ffi::types::GLuint,

    kawase_down_program: ffi::types::GLuint,
    kawase_down_u_tex: ffi::types::GLint,
    kawase_down_u_half_pixel: ffi::types::GLint,
    kawase_down_u_offset: ffi::types::GLint,
    kawase_down_a_position: ffi::types::GLint,

    kawase_blur_program: ffi::types::GLuint,
    kawase_blur_u_tex: ffi::types::GLint,
    kawase_blur_u_half_pixel: ffi::types::GLint,
    kawase_blur_u_offset: ffi::types::GLint,
    kawase_blur_a_position: ffi::types::GLint,

    backdrop_program: ffi::types::GLuint,
    backdrop_u_scene_tex: ffi::types::GLint,
    backdrop_u_blur_tex: ffi::types::GLint,
    backdrop_u_progress: ffi::types::GLint,
    backdrop_a_position: ffi::types::GLint,

    vbo: ffi::types::GLuint,
    initialized: bool,
}

impl QuickSettingsBackdropPipeline {
    /// Creates an uninitialized pipeline instance. Resources are allocated on first render.
    pub fn new() -> Self {
        Self {
            width: 0,
            height: 0,
            down_w: 0,
            down_h: 0,
            scene_tex: None,
            ping_fbo: 0,
            ping_tex: 0,
            blurred_fbo: 0,
            blurred_tex: 0,
            kawase_down_program: 0,
            kawase_down_u_tex: -1,
            kawase_down_u_half_pixel: -1,
            kawase_down_u_offset: -1,
            kawase_down_a_position: -1,
            kawase_blur_program: 0,
            kawase_blur_u_tex: -1,
            kawase_blur_u_half_pixel: -1,
            kawase_blur_u_offset: -1,
            kawase_blur_a_position: -1,
            backdrop_program: 0,
            backdrop_u_scene_tex: -1,
            backdrop_u_blur_tex: -1,
            backdrop_u_progress: -1,
            backdrop_a_position: -1,
            vbo: 0,
            initialized: false,
        }
    }

    /// Ensures that the offscreen scene texture has matching dimensions.
    ///
    /// # Mathematical Formulation
    /// Canvas target dimensions $W \times H$ (e.g. $412 \times 915$).
    /// Downsampled blur dimensions:
    /// $$W_{\text{down}} = \max(1, \lfloor W / 4 \rfloor) = 103$$
    /// $$H_{\text{down}} = \max(1, \lfloor H / 4 \rfloor) = 228$$
    pub fn ensure_scene_texture(
        &mut self,
        renderer: &mut GlesRenderer,
        width: i32,
        height: i32,
    ) -> Result<&mut GlesTexture, GlesError> {
        let needs_new = match &self.scene_tex {
            Some(tex) => tex.size() != (width, height).into(),
            None => true,
        };

        if needs_new {
            debug!("Allocating offscreen scene texture: {}x{}", width, height);
            let tex = renderer.create_buffer(Fourcc::Abgr8888, (width, height).into())?;
            self.scene_tex = Some(tex);
            self.initialized = false;
        }

        self.width = width;
        self.height = height;
        self.down_w = (width / DOWNSCALE_FACTOR).max(1);
        self.down_h = (height / DOWNSCALE_FACTOR).max(1);

        Ok(self.scene_tex.as_mut().unwrap())
    }

    /// Returns a reference to the offscreen scene texture if already allocated.
    pub fn scene_texture(&self) -> Option<&GlesTexture> {
        self.scene_tex.as_ref()
    }

    /// Initializes GL buffers, textures, and shader programs if not yet initialized
    /// or if dimensions have changed.
    unsafe fn ensure_gl_resources(&mut self, gl: &ffi::Gles2) -> Result<(), String> {
        if self.initialized {
            return Ok(());
        }

        info!(
            "Initializing QuickSettings GLES progressive blur pipeline (downscale {}x{} -> {}x{})",
            self.width, self.height, self.down_w, self.down_h
        );

        // 1. Compile Shaders
        let vert_shader = compile_shader(gl, ffi::VERTEX_SHADER, VERTEX_SHADER_SRC)?;
        let kawase_down_frag = compile_shader(gl, ffi::FRAGMENT_SHADER, KAWASE_DOWN_SRC)?;
        let kawase_blur_frag = compile_shader(gl, ffi::FRAGMENT_SHADER, KAWASE_BLUR_SRC)?;
        let backdrop_frag = compile_shader(gl, ffi::FRAGMENT_SHADER, BACKDROP_SRC)?;

        self.kawase_down_program = link_program(gl, vert_shader, kawase_down_frag)?;
        self.kawase_blur_program = link_program(gl, vert_shader, kawase_blur_frag)?;
        self.backdrop_program = link_program(gl, vert_shader, backdrop_frag)?;

        // Shaders attached to programs can be deleted now
        gl.DeleteShader(vert_shader);
        gl.DeleteShader(kawase_down_frag);
        gl.DeleteShader(kawase_blur_frag);
        gl.DeleteShader(backdrop_frag);

        // 2. Query Uniform and Attribute Locations for Kawase Down Program
        let u_tex_name = CString::new("u_tex").unwrap();
        let u_half_pixel_name = CString::new("u_half_pixel").unwrap();
        let u_offset_name = CString::new("u_offset").unwrap();
        let a_position_name = CString::new("a_position").unwrap();

        self.kawase_down_u_tex = gl.GetUniformLocation(self.kawase_down_program, u_tex_name.as_ptr());
        self.kawase_down_u_half_pixel = gl.GetUniformLocation(self.kawase_down_program, u_half_pixel_name.as_ptr());
        self.kawase_down_u_offset = gl.GetUniformLocation(self.kawase_down_program, u_offset_name.as_ptr());
        self.kawase_down_a_position = gl.GetAttribLocation(self.kawase_down_program, a_position_name.as_ptr());

        // Query Locations for Kawase Blur Program
        self.kawase_blur_u_tex = gl.GetUniformLocation(self.kawase_blur_program, u_tex_name.as_ptr());
        self.kawase_blur_u_half_pixel = gl.GetUniformLocation(self.kawase_blur_program, u_half_pixel_name.as_ptr());
        self.kawase_blur_u_offset = gl.GetUniformLocation(self.kawase_blur_program, u_offset_name.as_ptr());
        self.kawase_blur_a_position = gl.GetAttribLocation(self.kawase_blur_program, a_position_name.as_ptr());

        // 3. Query Uniform and Attribute Locations for Backdrop Program
        let u_scene_name = CString::new("u_scene_tex").unwrap();
        let u_blur_name = CString::new("u_blur_tex").unwrap();
        let u_prog_name = CString::new("u_progress").unwrap();

        self.backdrop_u_scene_tex = gl.GetUniformLocation(self.backdrop_program, u_scene_name.as_ptr());
        self.backdrop_u_blur_tex = gl.GetUniformLocation(self.backdrop_program, u_blur_name.as_ptr());
        self.backdrop_u_progress = gl.GetUniformLocation(self.backdrop_program, u_prog_name.as_ptr());
        self.backdrop_a_position = gl.GetAttribLocation(self.backdrop_program, a_position_name.as_ptr());

        // 4. Create VBO for Fullscreen Quad in NDC [-1, 1]
        let vertices: [f32; 8] = [
            -1.0, -1.0,
             1.0, -1.0,
            -1.0,  1.0,
             1.0,  1.0,
        ];
        gl.GenBuffers(1, &mut self.vbo);
        gl.BindBuffer(ffi::ARRAY_BUFFER, self.vbo);
        gl.BufferData(
            ffi::ARRAY_BUFFER,
            (vertices.len() * std::mem::size_of::<f32>()) as isize,
            vertices.as_ptr() as *const std::ffi::c_void,
            ffi::STATIC_DRAW,
        );
        gl.BindBuffer(ffi::ARRAY_BUFFER, 0);

        // 5. Create Downsampled Framebuffers & Textures
        if self.ping_fbo != 0 {
            gl.DeleteFramebuffers(1, &self.ping_fbo);
            self.ping_fbo = 0;
        }
        if self.ping_tex != 0 {
            gl.DeleteTextures(1, &self.ping_tex);
            self.ping_tex = 0;
        }
        if self.blurred_fbo != 0 {
            gl.DeleteFramebuffers(1, &self.blurred_fbo);
            self.blurred_fbo = 0;
        }
        if self.blurred_tex != 0 {
            gl.DeleteTextures(1, &self.blurred_tex);
            self.blurred_tex = 0;
        }

        let (ping_fbo, ping_tex) = create_fbo_and_texture(gl, self.down_w, self.down_h)?;
        self.ping_fbo = ping_fbo;
        self.ping_tex = ping_tex;

        let (blurred_fbo, blurred_tex) = create_fbo_and_texture(gl, self.down_w, self.down_h)?;
        self.blurred_fbo = blurred_fbo;
        self.blurred_tex = blurred_tex;

        self.initialized = true;
        Ok(())
    }

    /// Executes downsample pass, 2-pass Dual-Kawase blur, and progressive cross-fade backdrop.
    ///
    /// # Arguments
    /// * `gl` - OpenGL ES 2 interface
    /// * `scene_tex_id` - Texture ID of the captured sharp scene FBO
    /// * `progress` - Normalized drag progress $t \in [0.0, 1.0]$
    /// * `viewport_w` - Target width for the backdrop (e.g. 412)
    /// * `viewport_h` - Target height for the backdrop (e.g. 915)
    pub unsafe fn render_blur_and_backdrop(
        &mut self,
        gl: &ffi::Gles2,
        scene_tex_id: ffi::types::GLuint,
        progress: f64,
        viewport_w: i32,
        viewport_h: i32,
    ) {
        // Save caller's framebuffer binding, viewport, and rasterization state BEFORE anything else
        let mut prev_fbo = 0;
        gl.GetIntegerv(ffi::FRAMEBUFFER_BINDING, &mut prev_fbo);
        let mut prev_viewport = [0i32; 4];
        gl.GetIntegerv(ffi::VIEWPORT, prev_viewport.as_mut_ptr());
        let prev_blend = gl.IsEnabled(ffi::BLEND);
        let prev_scissor = gl.IsEnabled(ffi::SCISSOR_TEST);

        if let Err(e) = self.ensure_gl_resources(gl) {
            error!("Failed to initialize QuickSettings GL resources: {}", e);
            gl.BindFramebuffer(ffi::FRAMEBUFFER, prev_fbo as u32);
            return;
        }

        gl.Disable(ffi::BLEND);
        gl.Disable(ffi::SCISSOR_TEST);

        gl.BindBuffer(ffi::ARRAY_BUFFER, self.vbo);

        // =========================================================================
        // Pass 1: Downsample from full-res scene_tex (412x915) into ping_fbo (103x228)
        // =========================================================================
        gl.BindFramebuffer(ffi::FRAMEBUFFER, self.ping_fbo);
        gl.Viewport(0, 0, self.down_w, self.down_h);

        gl.UseProgram(self.kawase_down_program);
        gl.ActiveTexture(ffi::TEXTURE0);
        gl.BindTexture(ffi::TEXTURE_2D, scene_tex_id);
        gl.TexParameteri(ffi::TEXTURE_2D, ffi::TEXTURE_WRAP_S, ffi::CLAMP_TO_EDGE as i32);
        gl.TexParameteri(ffi::TEXTURE_2D, ffi::TEXTURE_WRAP_T, ffi::CLAMP_TO_EDGE as i32);
        gl.TexParameteri(ffi::TEXTURE_2D, ffi::TEXTURE_MIN_FILTER, ffi::LINEAR as i32);
        gl.TexParameteri(ffi::TEXTURE_2D, ffi::TEXTURE_MAG_FILTER, ffi::LINEAR as i32);
        gl.Uniform1i(self.kawase_down_u_tex, 0);
        gl.Uniform2f(
            self.kawase_down_u_half_pixel,
            0.5 / (self.width as f32),
            0.5 / (self.height as f32),
        );
        gl.Uniform1f(self.kawase_down_u_offset, 1.5);

        gl.EnableVertexAttribArray(self.kawase_down_a_position as u32);
        gl.VertexAttribPointer(
            self.kawase_down_a_position as u32,
            2,
            ffi::FLOAT,
            ffi::FALSE,
            0,
            std::ptr::null(),
        );
        gl.DrawArrays(ffi::TRIANGLE_STRIP, 0, 4);

        // =========================================================================
        // Pass 2: Dual-Kawase Dispersion Blur from ping_tex (103x228) into blurred_fbo (103x228)
        // =========================================================================
        gl.BindFramebuffer(ffi::FRAMEBUFFER, self.blurred_fbo);
        gl.Viewport(0, 0, self.down_w, self.down_h);

        gl.UseProgram(self.kawase_blur_program);
        gl.ActiveTexture(ffi::TEXTURE0);
        gl.BindTexture(ffi::TEXTURE_2D, self.ping_tex);
        gl.Uniform1i(self.kawase_blur_u_tex, 0);
        gl.Uniform2f(
            self.kawase_blur_u_half_pixel,
            0.5 / (self.down_w as f32),
            0.5 / (self.down_h as f32),
        );
        gl.Uniform1f(self.kawase_blur_u_offset, 2.0);

        gl.EnableVertexAttribArray(self.kawase_blur_a_position as u32);
        gl.VertexAttribPointer(
            self.kawase_blur_a_position as u32,
            2,
            ffi::FLOAT,
            ffi::FALSE,
            0,
            std::ptr::null(),
        );
        gl.DrawArrays(ffi::TRIANGLE_STRIP, 0, 4);

        // =========================================================================
        // Pass 3: Progressive Cross-Fade Pass onto Destination Framebuffer
        // =========================================================================
        gl.BindFramebuffer(ffi::FRAMEBUFFER, prev_fbo as u32);
        gl.Viewport(0, 0, viewport_w, viewport_h);

        gl.UseProgram(self.backdrop_program);

        // Unit 0: Sharp scene texture
        gl.ActiveTexture(ffi::TEXTURE0);
        gl.BindTexture(ffi::TEXTURE_2D, scene_tex_id);
        gl.Uniform1i(self.backdrop_u_scene_tex, 0);

        // Unit 1: Dispersed diffuse blur texture
        gl.ActiveTexture(ffi::TEXTURE1);
        gl.BindTexture(ffi::TEXTURE_2D, self.blurred_tex);
        gl.Uniform1i(self.backdrop_u_blur_tex, 1);

        gl.Uniform1f(self.backdrop_u_progress, progress.clamp(0.0, 1.0) as f32);

        gl.EnableVertexAttribArray(self.backdrop_a_position as u32);
        gl.VertexAttribPointer(
            self.backdrop_a_position as u32,
            2,
            ffi::FLOAT,
            ffi::FALSE,
            0,
            std::ptr::null(),
        );
        gl.DrawArrays(ffi::TRIANGLE_STRIP, 0, 4);

        // Cleanup state
        gl.DisableVertexAttribArray(self.backdrop_a_position as u32);
        gl.ActiveTexture(ffi::TEXTURE1);
        gl.BindTexture(ffi::TEXTURE_2D, 0);
        gl.ActiveTexture(ffi::TEXTURE0);
        gl.BindTexture(ffi::TEXTURE_2D, 0);
        gl.BindBuffer(ffi::ARRAY_BUFFER, 0);
        gl.UseProgram(0);

        // Restore framebuffer, viewport, and rasterization flags to caller's state
        gl.BindFramebuffer(ffi::FRAMEBUFFER, prev_fbo as u32);
        gl.Viewport(
            prev_viewport[0],
            prev_viewport[1],
            prev_viewport[2],
            prev_viewport[3],
        );
        if prev_blend == ffi::TRUE {
            gl.Enable(ffi::BLEND);
            gl.BlendFunc(ffi::ONE, ffi::ONE_MINUS_SRC_ALPHA);
        } else {
            gl.Disable(ffi::BLEND);
        }
        if prev_scissor == ffi::TRUE {
            gl.Enable(ffi::SCISSOR_TEST);
        } else {
            gl.Disable(ffi::SCISSOR_TEST);
        }
    }

    /// Releases all GPU textures, framebuffers, VBO, and compiled programs.
    pub unsafe fn destroy(&mut self, gl: &ffi::Gles2) {
        if !self.initialized {
            return;
        }

        if self.ping_fbo != 0 {
            gl.DeleteFramebuffers(1, &self.ping_fbo);
            self.ping_fbo = 0;
        }
        if self.ping_tex != 0 {
            gl.DeleteTextures(1, &self.ping_tex);
            self.ping_tex = 0;
        }
        if self.blurred_fbo != 0 {
            gl.DeleteFramebuffers(1, &self.blurred_fbo);
            self.blurred_fbo = 0;
        }
        if self.blurred_tex != 0 {
            gl.DeleteTextures(1, &self.blurred_tex);
            self.blurred_tex = 0;
        }
        if self.vbo != 0 {
            gl.DeleteBuffers(1, &self.vbo);
            self.vbo = 0;
        }
        if self.kawase_down_program != 0 {
            gl.DeleteProgram(self.kawase_down_program);
            self.kawase_down_program = 0;
        }
        if self.kawase_blur_program != 0 {
            gl.DeleteProgram(self.kawase_blur_program);
            self.kawase_blur_program = 0;
        }
        if self.backdrop_program != 0 {
            gl.DeleteProgram(self.backdrop_program);
            self.backdrop_program = 0;
        }

        self.scene_tex = None;
        self.initialized = false;
    }
}

impl Default for QuickSettingsBackdropPipeline {
    fn default() -> Self {
        Self::new()
    }
}

/// Helper function to compile a GLES shader.
unsafe fn compile_shader(
    gl: &ffi::Gles2,
    shader_type: ffi::types::GLenum,
    source: &str,
) -> Result<ffi::types::GLuint, String> {
    let shader = gl.CreateShader(shader_type);
    if shader == 0 {
        return Err("glCreateShader returned 0".to_string());
    }

    let length = source.len() as ffi::types::GLint;
    let ptr = source.as_ptr() as *const ffi::types::GLchar;
    gl.ShaderSource(shader, 1, &ptr, &length);
    gl.CompileShader(shader);

    let mut status = 0;
    gl.GetShaderiv(shader, ffi::COMPILE_STATUS, &mut status);
    if status == ffi::FALSE as i32 {
        let mut log_len = 0;
        gl.GetShaderiv(shader, ffi::INFO_LOG_LENGTH, &mut log_len);
        let mut buffer = vec![0u8; log_len as usize];
        gl.GetShaderInfoLog(
            shader,
            log_len,
            std::ptr::null_mut(),
            buffer.as_mut_ptr() as *mut ffi::types::GLchar,
        );
        let log = String::from_utf8_lossy(&buffer);
        gl.DeleteShader(shader);
        return Err(format!("Shader compilation failed: {log}"));
    }

    Ok(shader)
}

/// Helper function to link a GLES program.
unsafe fn link_program(
    gl: &ffi::Gles2,
    vert: ffi::types::GLuint,
    frag: ffi::types::GLuint,
) -> Result<ffi::types::GLuint, String> {
    let program = gl.CreateProgram();
    if program == 0 {
        return Err("glCreateProgram returned 0".to_string());
    }

    gl.AttachShader(program, vert);
    gl.AttachShader(program, frag);
    gl.LinkProgram(program);

    let mut status = 0;
    gl.GetProgramiv(program, ffi::LINK_STATUS, &mut status);
    if status == ffi::FALSE as i32 {
        let mut log_len = 0;
        gl.GetProgramiv(program, ffi::INFO_LOG_LENGTH, &mut log_len);
        let mut buffer = vec![0u8; log_len as usize];
        gl.GetProgramInfoLog(
            program,
            log_len,
            std::ptr::null_mut(),
            buffer.as_mut_ptr() as *mut ffi::types::GLchar,
        );
        let log = String::from_utf8_lossy(&buffer);
        gl.DeleteProgram(program);
        return Err(format!("Program link failed: {log}"));
    }

    Ok(program)
}

/// Helper function to create an FBO and texture pair.
unsafe fn create_fbo_and_texture(
    gl: &ffi::Gles2,
    width: i32,
    height: i32,
) -> Result<(ffi::types::GLuint, ffi::types::GLuint), String> {
    let mut tex = 0;
    gl.GenTextures(1, &mut tex);
    gl.BindTexture(ffi::TEXTURE_2D, tex);
    gl.TexParameteri(ffi::TEXTURE_2D, ffi::TEXTURE_WRAP_S, ffi::CLAMP_TO_EDGE as i32);
    gl.TexParameteri(ffi::TEXTURE_2D, ffi::TEXTURE_WRAP_T, ffi::CLAMP_TO_EDGE as i32);
    gl.TexParameteri(ffi::TEXTURE_2D, ffi::TEXTURE_MIN_FILTER, ffi::LINEAR as i32);
    gl.TexParameteri(ffi::TEXTURE_2D, ffi::TEXTURE_MAG_FILTER, ffi::LINEAR as i32);
    gl.TexImage2D(
        ffi::TEXTURE_2D,
        0,
        ffi::RGBA as i32,
        width,
        height,
        0,
        ffi::RGBA,
        ffi::UNSIGNED_BYTE,
        std::ptr::null(),
    );

    let mut fbo = 0;
    gl.GenFramebuffers(1, &mut fbo);
    gl.BindFramebuffer(ffi::FRAMEBUFFER, fbo);
    gl.FramebufferTexture2D(
        ffi::FRAMEBUFFER,
        ffi::COLOR_ATTACHMENT0,
        ffi::TEXTURE_2D,
        tex,
        0,
    );

    let status = gl.CheckFramebufferStatus(ffi::FRAMEBUFFER);
    gl.BindTexture(ffi::TEXTURE_2D, 0);

    if status != ffi::FRAMEBUFFER_COMPLETE {
        gl.DeleteFramebuffers(1, &fbo);
        gl.DeleteTextures(1, &tex);
        return Err(format!("Framebuffer incomplete: status {status:#x}"));
    }

    Ok((fbo, tex))
}
