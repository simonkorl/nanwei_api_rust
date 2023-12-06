#[macro_use]
extern crate log;

use nanwei_api_rust::ffi::*;
use nanwei_api_rust::server::DtpServer;
use nanwei_api_rust::DTP_API_MAP;
use nanwei_api_rust::{client::DtpClient, ffi::dtp_socket};
use std::sync::{Arc, Mutex};

#[tokio::main]
async fn main() {
    env_logger::init();
    // 模拟启动 server
    let server_handle = std::thread::spawn(|| {
        let config_ptr = dtp_config_init();
        dtp_config_set_max_idle_timeout(config_ptr, 10000);

        let sock = dtp_socket();

        info!("server sock {}", sock);

        dtp_bind(
            sock,
            std::ffi::CString::new("127.0.0.1").unwrap().as_ptr(),
            4433,
        );
        let conns_ptr = dtp_listen(sock, config_ptr);

        // join?
        let conns = unsafe { Box::from_raw(conns_ptr) };
        conns.join().unwrap();
    });

    // 模拟客户端程序
    let mut handles = vec![];
    for i in 0..500 {
        let h = tokio::spawn(async move {
            let config_ptr = dtp_config_init();
            dtp_config_set_max_idle_timeout(config_ptr, 3000);

            let sock = dtp_socket();
            info!("client sock {}", sock);
            let conn_io_ptr = dtp_connect(
                sock,
                std::ffi::CString::new("127.0.0.1").unwrap().as_ptr(),
                4433,
                config_ptr,
            );

            let conn_io = unsafe { Box::from_raw(conn_io_ptr) };

            conn_io.join().unwrap();
        });
        handles.push(h);
    }

    futures::future::join_all(handles).await;
    server_handle.join().unwrap();
}
