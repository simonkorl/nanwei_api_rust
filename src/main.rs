#[macro_use]
extern crate log;

use anyhow::anyhow;
use nanwei_api_rust::ffi::*;
use nanwei_api_rust::server::DtpServer;
use nanwei_api_rust::DTP_API_MAP;
use nanwei_api_rust::*;
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
        let ip = std::ffi::CString::new("127.0.0.1").unwrap();
        let ip_ptr = ip.as_ptr();
        dtp_bind(sock, ip_ptr, 4433);
        let conns_ptr = dtp_listen(sock, config_ptr);

        // join?
        let conns = unsafe { Box::from_raw(conns_ptr) };
        conns.join().unwrap();
        println!("server conns {} finished", sock);
    });

    // 模拟客户端程序
    let mut handles = vec![];
    for _ in 0..100 {
        let h = tokio::spawn(async move {
            let sock = dtp_socket();
            info!("client sock {}", sock);

            let conn_io = {
                let config_ptr = dtp_config_init();
                dtp_config_set_max_idle_timeout(config_ptr, 3000);

                let conn_io_ptr = {
                    let ip = std::ffi::CString::new("127.0.0.1").unwrap();
                    let ip_ptr = ip.as_ptr();
                    dtp_connect(sock, ip_ptr, 4433, config_ptr)
                };
                unsafe { Box::from_raw(conn_io_ptr) }
            };

            let tx = conn_io.tx.clone().unwrap();

            let hs = tokio::spawn(async move {
                // 模拟发送数据
                std::thread::sleep(std::time::Duration::from_secs(1));
                let msg = "hello world".to_owned().as_bytes().to_vec();
                match send(tx.clone(), msg.clone(), true, 8).await {
                    Ok(_) => eprintln!("{sock} sent hello world"),
                    Err(e) => eprintln!("{sock} failed to send hello world, may need to retry {e}"),
                }
            });

            hs.await.unwrap();
            conn_io.join().unwrap();
            println!("conn {} finished", sock);
        });
        handles.push(h);
    }

    futures::future::join_all(handles).await;
    server_handle.join().unwrap();
}
