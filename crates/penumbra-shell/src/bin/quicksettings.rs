mod implementation {
    include!("quick_settings.rs");
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    implementation::main()
}
