fn main() {
    slint_build::compile("ui/main.slint").expect("compile Slint user interface");

    #[cfg(windows)]
    {
        println!("cargo:rerun-if-changed=assets/app-icon.ico");
        winresource::WindowsResource::new()
            .set_icon("assets/app-icon.ico")
            .set("ProductName", "Ferro Code")
            .set("FileDescription", "Ferro Code")
            .compile()
            .expect("embed Windows application icon");
    }
}
