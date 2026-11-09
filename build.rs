fn main() {
    println!("cargo:rerun-if-changed=assets\\app.rc");
    println!("cargo:rerun-if-changed=assets\\CursorHighlighter.ico");
    println!("cargo:rerun-if-changed=app.manifest");
    embed_resource::compile("assets\\app.rc", embed_resource::NONE)
        .manifest_required()
        .expect("Windows SDK resource compiler is required to embed the icon and DPI manifest");
}
