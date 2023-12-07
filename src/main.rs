#[macro_use]
extern crate log;

use nanwei_api_rust::ffi::*;

use nanwei_api_rust::DTP_API_MAP;

use nanwei_api_rust::ffi::dtp_socket;
use std::env;

#[tokio::main]
async fn main() {
    env_logger::init();
    let client_num = {
        if env::args().len() >= 2 {
            let v: Vec<String> = env::args().collect();
            v[1].parse::<u32>().unwrap_or(250)
        } else {
            250
        }
    };
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
        let _conns = unsafe { Box::from_raw(conns_ptr) };
        let h = {
            let mut api_map = DTP_API_MAP.lock().unwrap();
            api_map
                .server_handles
                .remove(&sock)
                .expect(format!("no server handle for {sock}").as_str())
        };
        h.join().unwrap();
        println!("server conns {} finished", sock);
    });

    // 模拟客户端程序
    let mut handles = vec![];
    for _ in 0..client_num {
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

            let hs = std::thread::spawn(move || {
                // 模拟发送数据
                std::thread::sleep(std::time::Duration::from_secs(1));
                info!("enter thread {}", sock);
                let msg = "hello world".to_owned();
                let msg_ptr = msg.as_bytes().as_ptr();
                let conn_io = conn_io.clone();
                let conn_io_clone = conn_io.clone();
                let conn_io_ptr = Box::into_raw(conn_io_clone);
                loop {
                    info!("{} conn_io_ptr: {:?}", sock, conn_io_ptr);
                    match dtp_send(conn_io_ptr, msg_ptr, msg.len() as i32, true, 8) {
                        x if x >= 0 => {
                            info!("successfully send hello world {}", sock);
                            break;
                        }
                        -1 => {
                            info!("Done");
                            break;
                        }
                        -42 => {
                            info!("dtp_send need retry {}, retrying", sock);
                            std::thread::sleep(std::time::Duration::from_millis(100));
                        }
                        e => {
                            info!("failed to send msg in {}: {}", sock, e);
                            break;
                        }
                    }
                }
            });

            let h = {
                DTP_API_MAP
                    .lock()
                    .unwrap()
                    .client_handles
                    .remove(&sock)
                    .expect(format!("no client handle for {sock}").as_str())
            };
            hs.join().unwrap();
            // conn_io.join().unwrap();
            h.join().unwrap();
            info!("conn {} finished", sock);
        });
        handles.push(h);
    }

    futures::future::join_all(handles).await;
    server_handle.join().unwrap();
}
