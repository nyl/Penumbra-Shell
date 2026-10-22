mod implementation {
    include!("nav_bar.rs");
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    implementation::main()
}
