#[macro_use]
extern crate log;

use nanwei_api_rust::ffi::*;
use nanwei_api_rust::DtpConnection;

use nanwei_api_rust::DTP_API_MAP;

use nanwei_api_rust::ffi::dtp_socket;
use std::env;
use std::time::{Duration, Instant};

fn dtp_util_send(conn_io_ptr: *mut DtpConnection, stream_id: u64, send_data: &Vec<u8>) -> i32 {
    // encrypt
    let mut data_bytes: Vec<u8> = send_data.clone();
    data_bytes.insert(0, '+' as u8);

    info!(
        "开始发送数据总大小：{}, 数据:{:?}",
        data_bytes.len(),
        data_bytes
    );
    let start_millis = Instant::now();

    let sock = unsafe { conn_io_ptr.as_ref().unwrap().sockid };

    let send = loop {
        match dtp_send(
            conn_io_ptr,
            data_bytes.as_ptr(),
            data_bytes.len() as i32,
            true,
            stream_id as i32,
        ) {
            x if x >= 0 => {
                info!("successfully send in {:?}", sock);
                break x;
            }
            -1 => {
                info!("Done");
                break -1;
            }
            -42 => {
                info!("dtp_send need retry in sock {:?}, retrying...", sock);
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
            e => {
                info!("failed to send msg in sock {:?} : {}", sock, e);
                break e;
            }
        }
    };

    if (send as usize) < data_bytes.len() {
        warn!("send ({}) < data_bytes.len() {}", send, data_bytes.len());
    }

    let waker = unsafe { conn_io_ptr.as_mut().unwrap().waker.clone() };
    waker
        .lock()
        .unwrap()
        .wake()
        .expect("failed to wake thread in dtpSend");
    let end_millis = Instant::now();
    info!(
        "发送数据结束, 总发送大小:{},发送用时:{:?}",
        send,
        end_millis - start_millis
    );
    return send;
}

fn dtp_util_recv(conn_io_ptr: *mut DtpConnection) -> String {
    let start_millis = Instant::now();
    let mut buf = [0; 65535];

    let mut stream_id = 0;
    let mut fin = false;

    let sock = unsafe { conn_io_ptr.as_ref().unwrap().sockid };

    let mut loop_count = 0;

    let recv = loop {
        match dtp_recv(
            conn_io_ptr,
            buf.as_mut_ptr(),
            buf.len() as i32,
            &mut stream_id,
            &mut fin,
        ) {
            x if x > 0 => {
                info!("接收到的数据长度:{}, 接收到的streamId:{}", x, stream_id);
                break x;
            }
            0 => {
                if loop_count < 20 {
                    info!("{:?} 接收到的数据长度为 0 ，重试中。。。", sock);
                    std::thread::sleep(std::time::Duration::from_millis(100));
                    loop_count += 1;
                } else {
                    info!("{:?} 接收到的数据长度为 0 ，重试次数太多。", sock);
                    break 0;
                }
            }
            -1 => {
                info!("recv Done");
                break 0;
            }
            x if x < -1 => match dtp_connect_is_close(conn_io_ptr) {
                1 => {
                    info!("is closed in dtpRecv {:?}----", sock);
                    break -1;
                }
                0 => {
                    info!("{:?} not closed", sock);
                    break -1;
                }
                x if x < 0 => {
                    error!("{:?} dtpRecv error {x}", sock);
                    break -1;
                }
                e => {
                    error!("unexpect dtp_connect_is_close ret {e} in {:?}", sock);
                    break -1;
                }
            },
            e => {
                error!("unexpect dtpRecv ret {e} in {:?}", sock);
                break -1;
            }
        }
    };

    let end_millis = Instant::now();

    if recv > 0 {
        info!(
            "接收完成,数据总长度:{}, resultByte: {:?}, 用时:{:?}",
            recv,
            String::from_utf8(buf[..recv as usize].to_vec()).expect("failed"),
            end_millis - start_millis
        );
    } else {
        info!(
            "接收完成，但是没有有效数据。数据总长度:{}, 用时:{:?}",
            recv,
            end_millis - start_millis
        );
        return String::new();
    }
    // decrypt
    let res = buf[1..recv as usize].to_vec();

    return String::from_utf8(res).expect("failed to convert String in recv");
}

#[tokio::main]
async fn main() {
    env_logger::init();
    let client_num = {
        if env::args().len() >= 2 {
            let v: Vec<String> = env::args().collect();
            v[1].parse::<u32>().unwrap_or(250)
        } else {
            500
        }
    };

    // 模拟客户端程序
    let mut handles = vec![];
    for _ in 0..client_num {
        let h = tokio::spawn(async move {
            let (conn_io_ptr, sock) = {
                // getConn
                let sock = dtp_socket();
                info!("client sock {}", sock);

                let config_ptr = dtp_config_init();
                dtp_config_set_max_idle_timeout(config_ptr, 100000);

                let ip = std::ffi::CString::new("127.0.0.1").unwrap();
                let ip_ptr = ip.as_ptr();
                (dtp_connect(sock, ip_ptr, 4433, config_ptr), sock)
            };

            // dtp_util_send(conn_io_ptr, 8, &vec![sock as u8]);

            let msg = format!("{}", sock);

            let send = dtp_util_send(conn_io_ptr, 4, &msg.as_bytes().to_vec());
            info!("{} dtp_util_send, ret: {}", sock, send);
            let result = loop {
                std::thread::sleep(std::time::Duration::from_millis(10));
                match dtp_util_recv(conn_io_ptr) {
                    x if x.len() == 0 => {
                        debug!("keep recving response");
                    }
                    x if x.len() > 0 => {
                        info!("{} dtp_util_recv, ret: {:?}", sock, x);
                        break x;
                    }
                    e => {
                        info!("{} dtp_util_recv, ret: {:?}", sock, e);
                        break e;
                    }
                }
            };
            println!("{} dtp_util_recv, ret: {:?}", sock, result);
            // consume conn_io_ptr in dtp_close
            let ret = dtp_close(conn_io_ptr);
            if ret == 1 {
                info!("client {} closed", sock);
            } else {
                error!("client {} failed to close", sock);
            }

            info!("client {} finished", sock);
        });
        handles.push(h);
        std::thread::sleep(Duration::from_millis(10));
    }
    futures::future::join_all(handles).await;
}
