use mio::net::UdpSocket;
use quiche::{Config, Connection};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use ring::rand::*;

use anyhow::anyhow;
use anyhow::Result;

#[macro_use]
extern crate log;

struct DtpApi {
    sock_map: HashMap<u64, Option<UdpSocket>>,
    next_fd: u64, // 模拟产生的下一个 fd 编号
}

impl DtpApi {
    fn new() -> Self {
        DtpApi {
            sock_map: HashMap::new(),
            next_fd: 0,
        }
    }

    /// 假装创建了一个 fd，实际上没有分配任何 udp 资源
    fn create_udp_fd(&mut self) -> u64 {
        self.sock_map.insert(self.next_fd, None);
        self.next_fd += 1;
        self.next_fd
    }

    fn release_udp_fd(&mut self) {}

    fn bind(&mut self, sock: u64, ip: String, port: u32) -> Result<()> {
        if let Some(_) = self.sock_map.get(&sock) {
            Err(anyhow!("Already bind fd {}", sock))
        } else {
            let socket = UdpSocket::bind(format!("{}:{}", ip, port).parse()?)?;
            self.sock_map.insert(sock, Some(socket));
            Ok(())
        }
    }
}

pub mod ffi;
pub mod server;
pub mod client;