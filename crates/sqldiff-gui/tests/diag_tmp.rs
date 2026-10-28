//! TEMPORARY diagnostic for PR #128; removed before merge.
#![cfg(windows)]
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::os::windows::io::AsRawSocket;
use std::time::{Duration, Instant};

#[link(name = "kernel32")]
extern "system" {
    fn SetHandleInformation(h: *mut std::ffi::c_void, mask: u32, flags: u32) -> i32;
    fn GetHandleInformation(h: *mut std::ffi::c_void, flags: *mut u32) -> i32;
    fn GetLastError() -> u32;
}

fn flags(s: &TcpStream) -> String {
    let mut f = 0u32;
    let ok = unsafe { GetHandleInformation(s.as_raw_socket() as _, &mut f) };
    if ok == 0 { format!("err {}", unsafe { GetLastError() }) } else { format!("{f}") }
}

fn child() -> std::process::Child {
    std::process::Command::new("cmd")
        .args(["/C", "ping -n 4 127.0.0.1 >NUL"])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap()
}

/// clear: clear inherit flag after accept; clone: read through try_clone;
/// spawn: start a child while the connection is open.
fn case(clear: bool, clone: bool, spawn: bool) -> String {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = l.local_addr().unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let (mut s, _) = l.accept().unwrap();
        let mut info = format!("accepted={}", flags(&s));
        if clear {
            let r = unsafe { SetHandleInformation(s.as_raw_socket() as _, 1, 0) };
            info += &format!(" set={r}/{} after={}", unsafe { GetLastError() }, flags(&s));
        }
        let mut line = String::new();
        if clone {
            let c = s.try_clone().unwrap();
            info += &format!(" clone={}", flags(&c));
            let mut r = BufReader::new(c);
            loop { line.clear(); r.read_line(&mut line).unwrap(); if line == "\r\n" { break; } }
        } else {
            let mut r = BufReader::new(&s);
            loop { line.clear(); r.read_line(&mut line).unwrap(); if line == "\r\n" { break; } }
        }
        tx.send(info).unwrap();
        s.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok").unwrap();
    });
    let mut c = TcpStream::connect(addr).unwrap();
    std::thread::sleep(Duration::from_millis(300));
    let mut ch = if spawn { Some(child()) } else { None };
    let t = Instant::now();
    write!(c, "GET / HTTP/1.1\r\n\r\n").unwrap();
    let mut resp = String::new();
    let r = c.read_to_string(&mut resp);
    let took = t.elapsed();
    if let Some(ch) = ch.as_mut() { let _ = ch.kill(); let _ = ch.wait(); }
    let info = rx.recv_timeout(Duration::from_secs(1)).unwrap_or_default();
    format!("clear={clear} clone={clone} spawn={spawn}: {info} read={:?} took={took:?} len={}", r.err().map(|e| e.to_string()), resp.len())
}

#[test]
fn diag() {
    let mut out = String::new();
    for (clear, clone, spawn) in [
        (false, false, false), (false, false, true), (true, false, true),
        (false, true, true), (true, true, true), (true, true, false),
    ] {
        out += &case(clear, clone, spawn);
        out += "\n";
    }
    panic!("DIAG\n{out}");
}
