//! Background network image loading for `NodeKind::Image`'s `http://`/
//! `https://` sources.
//!
//! A GET request runs on its own `std::thread`, off the render thread — the
//! only blocking-async call in this codebase is `pollster`'s one-time wgpu
//! adapter negotiation (see `CLAUDE.md`'s third-party-crates section); a
//! network image fetch is a different, much longer-lived operation and has
//! no business blocking a 60fps redraw loop. The result is delivered back
//! through a plain `mpsc` channel, which `App` polls once per redraw
//! (`App::sync_network_image_loads`) — no async runtime pulled in.
//!
//! Per `CLAUDE.md`: "network images shouldn't be bundled as they are
//! dynamic" — a fetch is kicked off fresh every time the owning node is
//! (re)created (initial load, or a dynamic region re-expanding it), never
//! cached to disk or embedded at compile time.

use std::sync::mpsc::{channel, Receiver};
use std::thread;

/// Starts a GET request for `url` on a background thread and returns a
/// receiver that will yield exactly one `Result` once it completes (success
/// with decoded pixels, or a diagnostic string — a non-200 status, a
/// transport error, or a decode failure are all reported the same way,
/// surfaced by `NodeKind::Image::error`).
pub fn fetch(url: String) -> Receiver<Result<nowui_image::DecodedImage, String>> {
    let (tx, rx) = channel();
    thread::spawn(move || {
        let _ = tx.send(fetch_and_decode(&url));
    });
    rx
}

fn fetch_and_decode(url: &str) -> Result<nowui_image::DecodedImage, String> {
    let response = ureq::get(url).call().map_err(|e| format!("GET {url} failed: {e}"))?;
    let status = response.status();
    if status.as_u16() != 200 {
        return Err(format!("GET {url} returned HTTP {status}, expected 200"));
    }
    let bytes = response
        .into_body()
        .read_to_vec()
        .map_err(|e| format!("reading response body for {url}: {e}"))?;
    nowui_image::decode_bytes(&bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::time::Duration;

    /// Spins up a one-shot raw HTTP server on `127.0.0.1` (no framework —
    /// this crate has no test-server dependency and doesn't need one for a
    /// single canned response) that replies with `body` and `status` to
    /// exactly one request, then shuts down. Returns the bound address.
    fn serve_once(status_line: &'static str, content_type: &'static str, body: Vec<u8>) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral port");
        let addr = listener.local_addr().expect("local_addr");
        thread::spawn(move || {
            let Ok((mut stream, _)) = listener.accept() else { return };
            let mut buf = [0u8; 1024];
            let _ = stream.read(&mut buf);
            let header = format!("{status_line}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len());
            let _ = stream.write_all(header.as_bytes());
            let _ = stream.write_all(&body);
            let _ = stream.flush();
        });
        format!("http://{addr}")
    }

    fn recv(rx: Receiver<Result<nowui_image::DecodedImage, String>>) -> Result<nowui_image::DecodedImage, String> {
        rx.recv_timeout(Duration::from_secs(5)).expect("fetch thread never replied")
    }

    #[test]
    fn fetch_decodes_a_successful_200_response() {
        let mut img = image::RgbaImage::new(2, 2);
        for p in img.pixels_mut() {
            *p = image::Rgba([10, 20, 30, 255]);
        }
        let mut png_bytes = Vec::new();
        image::DynamicImage::ImageRgba8(img)
            .write_to(&mut std::io::Cursor::new(&mut png_bytes), image::ImageFormat::Png)
            .expect("encode test png");

        let url = serve_once("HTTP/1.1 200 OK", "image/png", png_bytes);
        let img = recv(fetch(url)).expect("expected a decoded image");
        assert_eq!((img.width, img.height), (2, 2));
        assert_eq!(img.frames.len(), 1);
    }

    #[test]
    fn fetch_reports_a_non_200_status_as_an_error() {
        let url = serve_once("HTTP/1.1 404 Not Found", "text/plain", b"nope".to_vec());
        let err = recv(fetch(url)).expect_err("expected an error for a 404");
        assert!(err.contains("404"), "error should mention the status code: {err}");
    }

    #[test]
    fn fetch_reports_undecodable_bytes_as_an_error() {
        let url = serve_once("HTTP/1.1 200 OK", "image/png", b"not actually a png".to_vec());
        let err = recv(fetch(url)).expect_err("expected a decode error");
        assert!(!err.is_empty());
    }
}
