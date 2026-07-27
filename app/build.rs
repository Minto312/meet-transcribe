fn main() {
    // Defer resolving openvino_genai_c.dll to first use.
    //
    // Without this the exe carries a load-time import, so Windows resolves it
    // before main() runs -- and on a machine where OpenVINO is not already on
    // PATH the process dies with 0xC00004BC and a bare
    // "openvino_genai_c.dll が見つかりません" dialog. Nothing the app could say
    // for itself, because none of its code has run yet.
    //
    // With the import delayed, main() gets to run first, locate the OpenVINO
    // install and prepend its bin directories to PATH (see app/src/openvino.rs).
    // The DLL is then resolved when the ASR thread first calls into it.
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        println!("cargo:rustc-link-arg-bins=/DELAYLOAD:openvino_genai_c.dll");
        // Provides __delayLoadHelper2, which the linker's delay stubs call.
        println!("cargo:rustc-link-lib=dylib=delayimp");
    }

    tauri_build::build()
}
