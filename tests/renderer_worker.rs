use std::io::{Read, Write};
use std::process::{Command, Stdio};

#[cfg(target_os = "linux")]
const MAX_WORKER_PEAK_RSS_KIB: u64 = 100 * 1024;

fn read_u32(reader: &mut impl Read) -> u32 {
    let mut bytes = [0_u8; 4];
    reader.read_exact(&mut bytes).unwrap();
    u32::from_be_bytes(bytes)
}

fn read_bytes(reader: &mut impl Read) -> Vec<u8> {
    let length = read_u32(reader) as usize;
    let mut bytes = vec![0_u8; length];
    reader.read_exact(&mut bytes).unwrap();
    bytes
}

fn write_request(writer: &mut impl Write, request: &[u8]) {
    writer
        .write_all(&(request.len() as u32).to_be_bytes())
        .unwrap();
    writer.write_all(request).unwrap();
    writer.flush().unwrap();
}

fn read_image_response(reader: &mut impl Read) -> (u32, u32, String, Vec<u8>) {
    let mut status = [0_u8; 1];
    reader.read_exact(&mut status).unwrap();
    assert_eq!(status[0], 0, "renderer worker returned an error response");
    assert_eq!(read_u32(reader), 1);
    let width = read_u32(reader);
    let height = read_u32(reader);
    let mime = String::from_utf8(read_bytes(reader)).unwrap();
    let png = read_bytes(reader);
    (width, height, mime, png)
}

#[cfg(target_os = "linux")]
fn process_peak_rss_kib(pid: u32) -> u64 {
    let status = std::fs::read_to_string(format!("/proc/{pid}/status")).unwrap();
    status
        .lines()
        .find_map(|line| {
            line.strip_prefix("VmHWM:")?
                .split_whitespace()
                .next()?
                .parse()
                .ok()
        })
        .expect("renderer worker VmHWM")
}

fn render_request(markdown: &str) -> Vec<u8> {
    serde_json::to_vec(&serde_json::json!({
        "markdown": markdown,
        "config": {
            "theme": "paper",
            "max_height": 2600,
            "font_size": 36,
            "code_font_size": 30,
            "padding": 64,
            "font": "",
            "title_font": "",
            "code_font": "",
            "emoji_font": ""
        }
    }))
    .unwrap()
}

#[test]
fn hidden_renderer_worker_returns_one_unicode_png() {
    let mut child = Command::new(env!("CARGO_BIN_EXE_miyu"))
        .arg("__renderer-worker")
        .env("MIYU_INTERNAL_RENDERER_WORKER", "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .unwrap();

    let cjk_request = render_request(
        r#"# Miyu 长图

纯中文、`inline code`。

```kdl
# ~/.config/kitty/kitty.conf
background_opacity 0.92
```

- [x] 已完成"#,
    );
    let mut stdin = child.stdin.take().unwrap();
    let mut stdout = child.stdout.take().unwrap();
    write_request(&mut stdin, &cjk_request);
    let (width, height, mime, png) = read_image_response(&mut stdout);

    assert!(width >= 960);
    assert!((360..=2600).contains(&height));
    assert_eq!(mime, "image/png");
    assert!(png.starts_with(b"\x89PNG\r\n\x1a\n"));
    let decoded = image::load_from_memory(&png).unwrap().to_rgba8();
    let code_surface_pixels = decoded
        .pixels()
        .filter(|pixel| **pixel == image::Rgba([225, 219, 208, 255]))
        .count();
    assert!(
        code_surface_pixels > width as usize * 40,
        "fenced code block did not produce a substantial light surface"
    );

    write_request(&mut stdin, &cjk_request);
    let second = read_image_response(&mut stdout);
    assert_eq!((second.0, second.1, second.2), (width, height, mime));
    assert!(second.3.starts_with(b"\x89PNG\r\n\x1a\n"));

    let table_request = render_request("| 名称 | 状态 |\n| --- | --- |\n| renderer | ready |");
    write_request(&mut stdin, &table_request);
    let table = read_image_response(&mut stdout);
    let table_image = image::load_from_memory(&table.3).unwrap().to_rgba8();
    let table_header = table_image.get_pixel(table.0 - 64 - 5, 64 + 5);
    assert!(
        table_header[0] > 200 && table_header[1] > 200 && table_header[2] > 200,
        "paper theme table header should remain light"
    );

    let emoji_request = render_request("# Emoji 按需加载\n\n中文和 Emoji 😀 可以一起渲染。");
    write_request(&mut stdin, &emoji_request);
    let emoji = read_image_response(&mut stdout);
    assert!(emoji.0 >= 960);
    assert!((360..=2600).contains(&emoji.1));
    assert_eq!(emoji.2, "image/png");
    assert!(emoji.3.starts_with(b"\x89PNG\r\n\x1a\n"));

    #[cfg(target_os = "linux")]
    {
        let peak_rss_kib = process_peak_rss_kib(child.id());
        assert!(
            peak_rss_kib < MAX_WORKER_PEAK_RSS_KIB,
            "renderer worker peak RSS was {peak_rss_kib} KiB, exceeding the {MAX_WORKER_PEAK_RSS_KIB} KiB budget"
        );
    }
    drop(stdin);
    assert!(child.wait().unwrap().success());
}
