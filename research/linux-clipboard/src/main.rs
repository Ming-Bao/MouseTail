//! Spike: read and write the Wayland clipboard from a windowless daemon via data-control,
//! cross-checked against wl-copy / wl-paste. Restores the previous clipboard text.

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("linux only");
}

#[cfg(target_os = "linux")]
fn main() {
    use std::io::Read;
    use std::process::Command;
    use std::time::Duration;

    use wl_clipboard_rs::copy::{self, MimeType as CopyMime, Source};
    use wl_clipboard_rs::paste::{self, ClipboardType, MimeType, Seat};

    let wl_paste = || {
        let out = Command::new("wl-paste")
            .arg("--no-newline")
            .output()
            .unwrap();
        String::from_utf8_lossy(&out.stdout).to_string()
    };
    let read = || -> String {
        let (mut pipe, _) =
            paste::get_contents(ClipboardType::Regular, Seat::Unspecified, MimeType::Text).unwrap();
        let mut s = String::new();
        pipe.read_to_string(&mut s).unwrap();
        s
    };

    let previous = wl_paste();

    let token = format!("mousetail-{}", std::process::id());
    let mut opts = copy::Options::new();
    opts.foreground(false);
    opts.copy(
        Source::Bytes(token.clone().into_bytes().into()),
        CopyMime::Text,
    )
    .unwrap();
    std::thread::sleep(Duration::from_millis(200));
    let seen = wl_paste();
    println!(
        "crate write -> wl-paste read: {}",
        if seen == token { "PASS" } else { "FAIL" }
    );

    let token2 = format!("{token}-back");
    Command::new("wl-copy")
        .arg(&token2)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .unwrap();
    std::thread::sleep(Duration::from_millis(200));
    let seen2 = read();
    println!(
        "wl-copy write -> crate read:  {}",
        if seen2 == token2 { "PASS" } else { "FAIL" }
    );

    // wl-copy forks a server that inherits stdio; detach it so callers aren't held open.
    Command::new("wl-copy")
        .arg(&previous)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .unwrap();
    println!("restored previous clipboard ({} bytes)", previous.len());
}
