use haveigotit::shell_paths::display_path;

fn units(value: &str) -> Vec<u16> {
    value.encode_utf16().collect()
}

#[test]
fn explorer_receives_drive_paths_without_the_verbatim_prefix() {
    assert_eq!(
        display_path(&units(r"\\?\C:\Users\Bob\Downloads\tool (1).zip")),
        Some(units(r"C:\Users\Bob\Downloads\tool (1).zip"))
    );
    assert_eq!(
        display_path(&units(r"D:\安装包\客户端（2）.zip")),
        Some(units(r"D:\安装包\客户端（2）.zip"))
    );
}

#[test]
fn chinese_spaces_and_non_bmp_characters_survive_shell_conversion() {
    let path = r"D:\安装 包\工具🧠（2）.zip";
    assert_eq!(
        display_path(&units(&format!(r"\\?\{path}"))),
        Some(units(path))
    );
    let mut path = units(r"\\?\C:\Downloads\");
    path.push(0xd800);
    path.extend(units(".zip"));
    assert_eq!(display_path(&path), Some(path[4..].to_vec()));
}

#[test]
fn unc_conversion_preserves_the_network_root() {
    assert_eq!(
        display_path(&units(r"\\?\UNC\server\share\工具.zip")),
        Some(units(r"\\server\share\工具.zip"))
    );
    assert_eq!(
        display_path(&units(r"\\?\unc\server\share\tool.zip")),
        Some(units(r"\\server\share\tool.zip"))
    );
}

#[test]
fn device_namespace_and_embedded_nul_are_not_misinterpreted() {
    assert!(display_path(&units(r"\\?\Volume{123}\tool.zip")).is_none());
    assert!(display_path(&units("C:\\Downloads\\tool\0.zip")).is_none());
}
