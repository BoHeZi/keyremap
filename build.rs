fn main() {
    // 链接预编译的资源文件, 提供 exe 图标和托盘图标 (资源名为 "id")
    if cfg!(target_os = "windows") {
        println!("cargo:rustc-link-arg=assets/app.res");
        println!("cargo:rerun-if-changed=assets/app.res");
    }
}
