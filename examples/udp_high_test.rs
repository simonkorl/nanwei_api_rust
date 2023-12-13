/// 测试 Udp socket 在高并发的场景下会如何表现
use mio::net::UdpSocket;
use rand::Rng;
use std::collections::HashMap;
use std::env;
use std::io;
use std::net::SocketAddr;
use std::time::Duration;
use threads_pool::ThreadPool;

#[derive(Default)]
struct LoopBuf {
    cache: HashMap<SocketAddr, Vec<u8>>,
}

impl LoopBuf {
    fn feed(&mut self, from: SocketAddr, buf: &[u8]) {
        if let Some(entry) = self.cache.get_mut(&from) {
            entry.append(&mut buf.to_vec());
        } else {
            self.cache.insert(from.clone(), buf.to_vec());
        }
    }

    fn get(&mut self, from: SocketAddr) -> Option<u64> {
        if let Some(entry) = self.cache.get_mut(&from) {
            if entry.len() >= 1200 {
                let mut num = [0u8; 8];
                num = entry[..8].try_into().unwrap();
                let no = u64::from_ne_bytes(num);
                *entry = entry[1200..].to_vec();
                Some(no)
            } else {
                None
            }
        } else {
            None
        }
    }

    fn get_first(&mut self) -> Option<(SocketAddr, u64)> {
        for (k, v) in self.cache.iter() {
            if v.len() >= 1200 {
                return Some((k.clone(), self.get(k.clone()).unwrap()));
            } else {
                continue;
            }
        }
        return None;
    }
}

fn would_block(err: &io::Error) -> bool {
    err.kind() == io::ErrorKind::WouldBlock
}

#[tokio::main]
async fn main() {
    env_logger::init();
    let client_num = {
        if env::args().len() >= 2 {
            let v: Vec<String> = env::args().collect();
            v[1].parse::<u32>().unwrap_or(250)
        } else {
            5
        }
    };
    // 模拟启动 server
    let server_handle = std::thread::spawn(move || {
        let mut server_socket = UdpSocket::bind("127.0.0.1:5862".parse().unwrap()).unwrap();
        let mut poll = mio::Poll::new().unwrap();
        let mut events = mio::Events::with_capacity(4096);
        let mut buf = [0; 2048];
        let mut loop_buf = LoopBuf::default();
        poll.registry()
            .register(&mut server_socket, mio::Token(0), mio::Interest::READABLE)
            .unwrap();
        loop {
            poll.poll(&mut events, Some(Duration::from_secs(5)))
                .unwrap();
            if events.is_empty() {
                println!("server timeout, exit!!");
                break;
            }
            // 接收数据 1200B，头部有一个 u64 的 id
            loop {
                match server_socket.recv_from(&mut buf) {
                    Ok((size, from)) => {
                        println!("server recv {}, {:?}", size, from);
                        loop_buf.feed(from, &buf[..size]);
                    }
                    Err(w) if would_block(&w) => {
                        break;
                    }
                    Err(e) => {
                        println!("server recv err {:?}", e);
                    }
                }
            }

            // 发送数据 1200B
            while let Some((from, client_no)) = loop_buf.get_first() {
                let msg = format!("Reply: server recved {}", client_no);
                buf[..8].copy_from_slice(&msg.len().to_ne_bytes());
                buf[8..8 + msg.len()].copy_from_slice(msg.as_bytes());
                match server_socket.send_to(&buf[..1200], from) {
                    Ok(size) => println!(
                        "server replied {} successfully, {}",
                        size,
                        String::from_utf8(buf[8..8 + msg.len()].to_vec()).unwrap()
                    ),
                    Err(e) => println!("server failed to reply to {} due to {}", client_no, e),
                }
            }
        }
    });

    // 模拟客户端程序
    let mut handles = vec![];
    for i in 0..client_num {
        handles.push(std::thread::spawn(move || {
            // pool.execute(move || {
            let mut client_socket = UdpSocket::bind("0.0.0.0:0".parse().unwrap()).unwrap();
            // client_socket
            //     .connect("127.0.0.1:5862".parse().unwrap())
            //     .unwrap();
            let mut poll = mio::Poll::new().unwrap();
            let mut events = mio::Events::with_capacity(2048);
            let mut buf = [0; 2048];
            poll.registry()
                .register(&mut client_socket, mio::Token(0), mio::Interest::READABLE)
                .unwrap();
            // 发送数据 1200B
            let bytes = (i as u64).to_ne_bytes();
            buf[..8].clone_from_slice(&bytes);
            match client_socket.send_to(&buf[..1200], "127.0.0.1:5862".parse().unwrap()) {
                Ok(size) => println!("client {} retrying, send {}", i, size),
                Err(e) => println!("client {} retry send err {:?}", i, e),
            };
            let mut get_response = false;
            let mut rng = rand::thread_rng();
            while !get_response {
                let random_sleep_time = rng.gen_range(0..100);
                poll.poll(
                    &mut events,
                    Some(std::time::Duration::from_secs(random_sleep_time)),
                )
                .unwrap();

                if events.is_empty() {
                    // 发送数据 1200B
                    let bytes = (i as u64).to_ne_bytes();
                    buf[..8].clone_from_slice(&bytes);
                    match client_socket.send_to(&buf[..1200], "127.0.0.1:5862".parse().unwrap()) {
                        Ok(size) => println!("client {} send {}", i, size),
                        Err(e) => println!("client {} send err {:?}", i, e),
                    };
                }

                for e in events.iter() {
                    match e.token() {
                        mio::Token(0) => {
                            // 接收数据 1200B
                            match client_socket.recv(&mut buf) {
                                Ok(size) => {
                                    let str_len =
                                        usize::from_ne_bytes(buf[..8].try_into().unwrap());
                                    println!(
                                        "client {} recv {} of {}",
                                        i,
                                        size,
                                        String::from_utf8(buf[8..8 + str_len].to_vec()).unwrap()
                                    )
                                }
                                Err(e) => println!("client {} recv err {:?} ", i, e),
                            }
                            get_response = true;
                        }
                        _ => {
                            println!("unknow token");
                        }
                    }
                }
            }
        }));
        // .expect("failed to execute");
    }

    for h in handles {
        h.join().unwrap();
    }
    server_handle.join().unwrap();
}
