fn main() {
    glib_build_tools::compile_resources(
        &["resources"],
        "resources/atoll.gresource.xml",
        "atoll.gresource",
    );
}
