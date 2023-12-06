use futures::future::Join;
use lazy_static::lazy_static;
use libc::c_int;
use mio::net::UdpSocket;

use std::collections::HashMap;

use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use crate::client::DtpClient;
use crate::server::DtpServer;

use anyhow::anyhow;
use anyhow::Result;

#[macro_use]
extern crate log;

#[repr(C)]
#[derive(Default)]
/// 模拟的 conn_io 结构体
/// 其中的结构和之前实现的 conn_io 完全不同
pub struct DtpConnection {
    client: Option<Arc<Mutex<DtpClient>>>,
    handle: Option<JoinHandle<()>>,
}

impl DtpConnection {
    pub fn join(self) -> Result<()> {
        if let Some(h) = self.handle {
            if !h.is_finished() {
                match h.join() {
                    Ok(_) => Ok(()),
                    Err(e) => Err(anyhow!("{:?}", e)),
                }
            } else {
                Ok(())
            }
        } else {
            Ok(())
        }
    }
}
#[repr(C)]
#[derive(Default)]
pub struct DtpServerConns {
    server: Option<Arc<Mutex<DtpServer>>>,
    handle: Option<JoinHandle<()>>,
}

impl DtpServerConns {
    pub fn join(self) -> Result<()> {
        if let Some(h) = self.handle {
            if !h.is_finished() {
                match h.join() {
                    Ok(_) => Ok(()),
                    Err(e) => Err(anyhow!("{:?}", e)),
                }
            } else {
                Ok(())
            }
        } else {
            Ok(())
        }
    }
}

#[derive(Default)]
pub struct DtpApi {
    /// 为了契合 tcp api 而做的一个模拟 sockfd 的功能
    ///
    /// 如果 sock_map 的 value 为 None，说明这个 sock 绑定的是一个 client
    ///
    /// 如果此时 client 已经成功连接，那么可以在 client_map 中找到对应的对象
    /// 读取对于对应的指针即可
    ///
    /// 如果此时 client 还没有连接，那么我们禁止返回这个 sock
    ///
    /// 目前这个库不支持 C 一样的 sock 操作，因为可能出 bug
    pub sock_map: HashMap<c_int, Option<Arc<Mutex<UdpSocket>>>>,
    pub client_map: HashMap<c_int, Arc<Mutex<DtpClient>>>,
    pub server_map: HashMap<c_int, Arc<Mutex<DtpServer>>>,
    pub server_handles: HashMap<c_int, std::thread::JoinHandle<()>>,
    pub client_handles: HashMap<c_int, std::thread::JoinHandle<()>>,
    next_fd: c_int, // 模拟产生的下一个 fd 编号
}

impl DtpApi {
    /// 假装创建了一个 fd，实际上没有分配任何 udp 资源
    /// 这个函数会在 sock_map 中插入一个值，代表已经初始化 socket ，但是没有赋值
    fn create_udp_fd(&mut self) -> c_int {
        let fd = self.next_fd;
        self.sock_map.insert(fd, None);
        self.next_fd += 1;
        fd
    }

    fn release_udp_fd(&mut self) {}

    fn has_server(&self, sock: c_int) -> bool {
        self.server_map.get(&sock).is_some()
    }

    fn has_client(&self, sock: c_int) -> bool {
        self.client_map.get(&sock).is_some()
    }

    fn has_sock(&self, sock: c_int) -> bool {
        if let Some(opt) = self.sock_map.get(&sock) {
            opt.is_some()
        } else {
            false
        }
    }

    fn has_sock_entry(&self, sock: c_int) -> bool {
        self.sock_map.get(&sock).is_some()
    }

    // fn bind(&mut self, sock: c_int, ip: String, port: c_int) -> Result<()> {
    //     if let Some(_) = self.sock_map.get(&sock) {
    //         Err(anyhow!("Already bind fd {}", sock))
    //     } else {
    //         let socket = UdpSocket::bind(format!("{}:{}", ip, port).parse()?)?;
    //         self.sock_map.insert(sock, Some(socket));
    //         Ok(())
    //     }
    // }
}

lazy_static! {
    /// 一个全局变量
    ///
    /// 用于储存全局 api 数据
    /// 并与 ffi api 交互
    ///
    pub static ref DTP_API_MAP: Arc<Mutex<DtpApi>> = Arc::new(Mutex::new(DtpApi::default()));
}

pub mod client;
pub mod ffi;
pub mod server;
