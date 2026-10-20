slint::slint! {
    export component MinimalBar inherits Window {
        height: 42px;
        background: #1e1e2e;
        
        HorizontalLayout {
            padding: 8px;
            alignment: space-between;
            
            Text {
                text: "Penumbra Layer Shell";
                color: #cdd6f4;
                font-size: 14px;
                vertical-alignment: center;
            }
            
            Rectangle {
                width: 80px;
                height: 28px;
                background: #89b4fa;
                border-radius: 14px;
                
                Text {
                    text: "Action";
                    color: #11111b;
                    font-size: 12px;
                    font-weight: 700;
                    horizontal-alignment: center;
                    vertical-alignment: center;
                }
                
                TouchArea {
                    clicked => {
                        debug("Slint Layer Shell touch/click registered!");
                    }
                }
            }
        }
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    env_logger::init();
    
    // Initialize custom layer shell platform for a 42px top bar
    let config = slint_layer_shell::LayerConfig::top_bar(42);
    slint_layer_shell::init(config)?;
    
    let bar = MinimalBar::new()?;
    bar.run()?;
    Ok(())
}
