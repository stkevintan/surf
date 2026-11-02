//! Can this process build the socket pair libzmq's signaler needs?
//!
//! Kontakt aborts at `epoll.cpp:73`, which is `errno_assert` on
//! `epoll_ctl (EPOLL_CTL_ADD)` returning EBADF — i.e. libzmq handed wepoll a
//! file descriptor that was never a valid socket. libzmq gets that descriptor
//! from `zmq::signaler_t`, which on Windows builds a self-connected pair over
//! AF_UNIX (preferred, when the OS has it) or loopback TCP (fallback). If the
//! pair fails, the retired descriptor is what reaches `add_fd`.
//!
//! Standalone Kontakt loads `wshunix.dll` (the AF_UNIX Winsock provider) and
//! works; our host process never loads it and dies here. So this probe asks
//! plainly whether *this* process can make each kind of pair.
//!
//! ```powershell
//! cargo run --example fdpair_probe
//! ```

use std::mem::{size_of, zeroed};

use windows::Win32::Networking::WinSock::{
    AF_INET, AF_UNIX, INVALID_SOCKET, IPPROTO_TCP, SOCK_STREAM, SOCKADDR, SOCKADDR_IN,
    SOCKET_ERROR, WSADATA, WSAGetLastError, WSAStartup, bind, closesocket, connect, getsockname,
    listen, socket,
};

/// Build a loopback TCP pair the way libzmq's fallback does: listen on an
/// ephemeral port, connect to it, and keep both ends.
fn tcp_pair() -> Result<(), String> {
    // SAFETY: every call below is a plain Winsock call on locals we own.
    unsafe {
        // This binding returns Result, and still yields INVALID_SOCKET inside Ok
        // on some paths, so check both.
        let listener = socket(AF_INET.0 as i32, SOCK_STREAM, IPPROTO_TCP.0)
            .map_err(|e| format!("socket(AF_INET) failed: {e}"))?;
        if listener == INVALID_SOCKET {
            return Err(format!("socket(AF_INET) invalid: {:?}", WSAGetLastError()));
        }
        let mut addr: SOCKADDR_IN = zeroed();
        addr.sin_family = AF_INET;
        // Port 0 = let the stack pick; loopback only, like libzmq.
        addr.sin_addr.S_un.S_addr = u32::from_ne_bytes([127, 0, 0, 1]);
        if bind(
            listener,
            &addr as *const _ as *const SOCKADDR,
            size_of::<SOCKADDR_IN>() as i32,
        ) == SOCKET_ERROR
        {
            let err = WSAGetLastError();
            closesocket(listener);
            return Err(format!("bind(127.0.0.1:0) failed: {err:?}"));
        }
        if listen(listener, 1) == SOCKET_ERROR {
            let err = WSAGetLastError();
            closesocket(listener);
            return Err(format!("listen failed: {err:?}"));
        }
        let mut bound: SOCKADDR_IN = zeroed();
        let mut len = size_of::<SOCKADDR_IN>() as i32;
        if getsockname(listener, &mut bound as *mut _ as *mut SOCKADDR, &mut len) == SOCKET_ERROR {
            let err = WSAGetLastError();
            closesocket(listener);
            return Err(format!("getsockname failed: {err:?}"));
        }
        let client = match socket(AF_INET.0 as i32, SOCK_STREAM, IPPROTO_TCP.0) {
            Ok(sock) if sock != INVALID_SOCKET => sock,
            other => {
                closesocket(listener);
                return Err(format!("client socket failed: {other:?}"));
            }
        };
        let rc = connect(
            client,
            &bound as *const _ as *const SOCKADDR,
            size_of::<SOCKADDR_IN>() as i32,
        );
        let result = if rc == SOCKET_ERROR {
            Err(format!("connect to loopback failed: {:?}", WSAGetLastError()))
        } else {
            Ok(())
        };
        closesocket(client);
        closesocket(listener);
        result
    }
}

/// Does this process get an AF_UNIX socket at all? Creating one is what pulls
/// `wshunix.dll` into a process.
fn unix_socket() -> Result<(), String> {
    // SAFETY: plain Winsock socket creation; handle is closed on both paths.
    unsafe {
        // AF_UNIX is a bare u16 in this binding, unlike AF_INET.
        let sock = socket(AF_UNIX as i32, SOCK_STREAM, 0)
            .map_err(|e| format!("socket(AF_UNIX) failed: {e}"))?;
        if sock == INVALID_SOCKET {
            return Err(format!("socket(AF_UNIX) invalid: {:?}", WSAGetLastError()));
        }
        closesocket(sock);
        Ok(())
    }
}

fn main() {
    // SAFETY: WSADATA is a POD out-param; 2.2 is the standard request.
    let rc = unsafe {
        let mut data: WSADATA = zeroed();
        WSAStartup(0x0202, &mut data)
    };
    println!("WSAStartup -> {rc}");

    match tcp_pair() {
        Ok(()) => println!("loopback TCP pair: OK"),
        Err(err) => println!("loopback TCP pair: FAILED — {err}"),
    }
    match unix_socket() {
        Ok(()) => println!("AF_UNIX socket: OK"),
        Err(err) => println!("AF_UNIX socket: FAILED — {err}"),
    }
}
