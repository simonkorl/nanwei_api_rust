use libc::c_char;
use libc::c_int;

use std::ffi;

use crate::client::*;

use crate::server::DtpServer;
use crate::server::*;
use crate::DTP_API_MAP;
use crate::{DtpConnection, DtpServerConns};
use quiche::Config;
use quiche::Error;
use std::sync::{Arc, Mutex};

type c_bool = bool;

fn error_to_c(e: Error) -> libc::ssize_t {
    match e {
        Error::Done => -1,
        Error::BufferTooShort => -2,
        Error::UnknownVersion => -3,
        Error::InvalidFrame => -4,
        Error::InvalidPacket => -5,
        Error::InvalidState => -6,
        Error::InvalidStreamState(_) => -7,
        Error::InvalidTransportParam => -8,
        Error::CryptoFail => -9,
        Error::TlsFail => -10,
        Error::FlowControl => -11,
        Error::StreamLimit => -12,
        Error::FinalSize => -13,
        Error::CongestionControl => -14,
        Error::StreamStopped { .. } => -15,
        Error::StreamReset { .. } => -16,
    }
}
const QUICHE_PROTOCOL_VERSION: u32 = 0x00000001;

#[repr(C)]
pub enum DtpError {
    DtpNotEstablished = -42,
    DtpNull = -404,
    DtpDefaultErr = -444,
}

//----------API接口函数声明---------
//生成默认的客户端和服务端的配置文件
#[no_mangle]
pub extern "C" fn dtp_config_init() -> *mut Config {
    let mut config = match Config::new(QUICHE_PROTOCOL_VERSION) {
        Ok(c) => Box::new(c),

        Err(e) => {
            error!("failed to create config {:?}", e);
            return std::ptr::null_mut();
        }
    };

    match config.load_cert_chain_from_pem_file("./cert.crt") {
        Ok(_) => (),
        Err(e) => {
            error!("failed to read cert.crt {:?}", e);
            return std::ptr::null_mut();
        }
    };

    match config.load_priv_key_from_pem_file("./cert.key") {
        Ok(_) => (),
        Err(e) => {
            error!("failed to read cert.key {:?}", e);
            return std::ptr::null_mut();
        }
    };

    if let Err(e) = config.set_application_protos(b"\x05hq-28\x05hq-27\x08http/0.9") {
        error!("failed to set application protocols {:?}", e);
        return std::ptr::null_mut();
    };

    config.set_max_idle_timeout(1000000); // 设置一个超长的超时时间，防止极长的数据处理时间带来不可预测的结果
    config.set_initial_max_data(0xffffffff);
    config.set_initial_max_stream_data_bidi_local(0xffffffff);
    config.set_initial_max_stream_data_bidi_remote(0xffffffff);
    config.set_initial_max_streams_bidi(0xffffffff);
    config.set_cc_algorithm(quiche::CongestionControlAlgorithm::Reno);
    config.set_disable_active_migration(true);
    // quiche_config_set_max_packet_size(config, MAX_DATAGRAM_SIZE);

    if let Ok(_) = std::env::var("SSLKEYLOGFILE") {
        config.log_keys();
    }

    return Box::into_raw(config);
}

/**
 * 创建套接字
 * 创建一个非阻塞的，地址可复用的udp；和tcp中的bind无异
 * 成功返回>0的sock，失败返回-1
*/
#[no_mangle]
pub extern "C" fn dtp_socket() -> c_int {
    debug!("enter dtp_socket");
    return DTP_API_MAP.lock().unwrap().create_udp_fd();
}

/**
 * @brief 在调用 dtp_socket 之后发生其他的意外时进行调用，关闭 dtp_socket
 *
 * @param sock
 * @return int =0 成功，其他都是失败
 */
#[no_mangle]
pub extern "C" fn dtp_socket_close(sock: c_int) -> c_int {
    let mut api_map = DTP_API_MAP.lock().unwrap();
    if !api_map.has_sock_entry(sock) {
        error!("failed to close dtp_socket: no such sock, {}", sock);
        return -1;
    }
    if api_map.has_client(sock) {
        error!("failed to close dtp_socket: sock in use (client), {}", sock);
        return -1;
    }
    if api_map.has_server(sock) {
        error!("failed to close dtp_socket: sock in use (server), {}", sock);
        return -1;
    }
    api_map.sock_map.remove(&sock);
    return 0;
}

/*
绑定端口
给套接字绑定一个地址；和tcp中的bind无异
成功返回1，失败返回-1
只有 server 会调用这个函数
*/
#[no_mangle]
pub extern "C" fn dtp_bind(sock: c_int, ip: *const c_char, port: c_int) -> c_int {
    let mut api_map = DTP_API_MAP.lock().unwrap();

    if !api_map.has_sock_entry(sock) {
        error!("error in dtp_bind: no such sock {}", sock);
        return -1;
    }
    if api_map.has_server(sock) {
        error!("error in dtp_bind: sock {} already binded", sock);
        return -1;
    }

    let ip = unsafe { ffi::CStr::from_ptr(ip).to_str().unwrap() };
    let server = match DtpServer::bind(ip.to_owned(), port, sock) {
        Ok(s) => s,
        Err(e) => {
            error!("error in dtp_bind: {:?}", e);
            return -1;
        }
    };

    api_map.sock_map.insert(sock, Some(server.get_socket()));
    api_map.server_map.insert(sock, server);
    return 1;
}

/// 监听端口
/// 设置监听端口
/// 非阻塞
/// 成功返回一个会话地址，失败返回NULL
/// 只有 server 会调用这个函数
/// !dtp_listen 会 consume 掉 config 。它不需要再进行释放，同时也不能再次被使用。
#[no_mangle]
pub extern "C" fn dtp_listen(sock: c_int, config: *mut Config) -> *mut DtpServerConns {
    let mut api_map = DTP_API_MAP.lock().unwrap();
    if !api_map.has_sock_entry(sock) {
        error!("error in dtp_listen: no such sock {}", sock);
        return std::ptr::null_mut();
    } else if !api_map.has_server(sock) {
        error!("error in dtp_listen: sock {} not binded", sock);
        return std::ptr::null_mut();
    }
    // 在 dtp_bind 的基础上为 DtpServer 赋予 config 的值
    // 之后该结构体就可以运行 run 函数
    let mut server = {
        if let Some(s) = api_map.server_map.get_mut(&sock) {
            let config = unsafe { Box::from_raw(config) };
            let config = Arc::new(Mutex::new(*config));
            s.set_config(config);
            api_map.server_map.remove(&sock).unwrap()
        } else {
            error!("error in dtp_listen: sock is not binded, {}", sock);
            return std::ptr::null_mut();
        }
    };

    let streams = server.get_shared_streams();
    let waker = server.get_waker();
    let waker_clone = waker.clone();

    // 使用线程来运行 DtpServer 以及相关的程序
    let h = std::thread::spawn(move || {
        server.run().unwrap();
    });

    api_map.server_handles.insert(sock, h);
    api_map.server_shared_map.insert(sock, streams.clone());

    let conns = DtpServerConns {
        // server: server_arc.clone(),
        waker,
        streams,
        sockid: sock,
    };

    api_map.server_waker_map.insert(sock, waker_clone);

    return Box::into_raw(Box::new(conns));
}

/// 接受链接
/// is_block：是否阻塞；false = 非阻塞
/// 成功返回一个地址，失败返回NULL
/// 只有 server 会调用这个函数
#[no_mangle]
pub extern "C" fn dtp_accept(
    conns: *mut DtpServerConns,
    is_block: c_bool,
) -> *mut DtpConnection {
    let conns = unsafe {
        if let Some(c) = conns.as_mut() {
            c
        } else {
            error!("error in dtp_accept: conns is null");
            return std::ptr::null_mut();
        }
    };
    let (shared_streams, waker) = (conns.streams.clone(), conns.waker.clone());
    debug!("before dtp_accept loop");
    loop {
        {
            let client_status = &mut shared_streams.lock().unwrap().client_status;
            debug!("get client_status");
            for (scid, (conn, status)) in client_status.iter_mut() {
                debug!("accepting {:?}", scid);
                if status == &mut ClientStatus::Connected {
                    *status = ClientStatus::Accepted;
                    let conn_io = DtpConnection {
                        conn: conn.clone(),
                        waker,
                        is_server_side: true,
                        sockid: None,
                        pconns: Some(shared_streams.clone()),
                        conn_id: Some(scid.clone()),
                    };
                    return Box::into_raw(Box::new(conn_io));
                }
            }
        }
        if is_block {
            std::thread::sleep(std::time::Duration::from_millis(100));
        } else {
            break;
        }
    }

    return std::ptr::null_mut();
}

/// 发起连接
/// 成功返回地址，失败返回NULL
/// 只有 client 会调用这个函数
#[no_mangle]
pub extern "C" fn dtp_connect(
    sock: c_int,
    ip: *const c_char,
    port: c_int,
    config: *mut Config,
) -> *mut DtpConnection {
    let config = unsafe {
        if let Some(c) = config.as_mut() {
            c
        } else {
            error!("error in dtp_connect: Config is null");
            return std::ptr::null_mut();
        }
    };

    let mut api_map = DTP_API_MAP.lock().unwrap();
    if !api_map.has_sock_entry(sock) {
        error!("error in dtp_connect: no such sock {}", sock);
        return std::ptr::null_mut();
    }
    if api_map.has_client(sock) {
        error!("error in dtp_connect: sock {} already connected", sock);
        return std::ptr::null_mut();
    }
    let ip = unsafe { ffi::CStr::from_ptr(ip).to_str().unwrap() };

    // 创建 client，它的所有权会被运行线程获得
    // TODO: 在 client 退出的时候能否正确地释放数据？
    let mut client = DtpClient::connect(ip.to_owned(), port, config, sock).unwrap();

    // let conn = client_clone.conn.clone();
    let conn_clone = client.get_conn();
    // let waker = client_clone.waker.clone();
    let waker_clone = client.get_waker();
    let socket = client.get_socket();
    api_map.sock_map.insert(sock, Some(socket));

    // 运行 client 的线程
    let h = std::thread::spawn(move || {
        client.run(sock).unwrap();
        info!("client main loop stopped {}", sock);
    });

    api_map.client_handles.insert(sock, h);

    // 等待连接完全建立再返回，以防出现难以预测的情况
    // ? 不过不确定是否可以得到正确的结果
    let conn_wait = conn_clone.clone();
    loop {
        let is_established = conn_wait.lock().unwrap().is_established();
        let is_in_early_data = conn_wait.lock().unwrap().is_in_early_data();
        if is_established || is_in_early_data {
            break;
        } else {
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
    }

    let conn_io = DtpConnection {
        // client: Some(client_arc.clone()),
        conn: conn_clone,
        waker: waker_clone,
        is_server_side: false,
        // handle: Some(h),
        // msg_handle: Some(hm),
        sockid: Some(sock),
        // tx: Some(tx),
        pconns: None,
        conn_id: None,
    };

    return Box::into_raw(Box::new(conn_io));
}

/*
接受数据，这个函数是非阻塞的
成功返回接受的长度，失败返回-1
新增 uin64_t streamid 和bool fin；需要传入指针。streamid是标识收到的数据是从哪个流中获得。fin表示这个流是否关闭
当返回-1的时候，建议对链接判断是否关闭。
*/
#[no_mangle]
pub extern "C" fn dtp_recv(
    conn_io: *mut DtpConnection,
    buf: *mut u8,
    buflen: c_int,
    stream_id: *mut c_int,
    fin: *mut c_bool,
) -> c_int {
    let conn_io = unsafe {
        if let Some(c) = conn_io.as_ref() {
            c
        } else {
            return DtpError::DtpNull as c_int;
        }
    };

    let stream_id = unsafe {
        if let Some(s) = stream_id.as_mut() {
            s
        } else {
            return DtpError::DtpNull as c_int;
        }
    };

    let fin = unsafe {
        if let Some(s) = fin.as_mut() {
            s
        } else {
            return DtpError::DtpNull as c_int;
        }
    };

    let mut buf = unsafe { std::slice::from_raw_parts_mut(buf, buflen as usize) };

    // 调用 api 接收
    let conn = conn_io.conn.clone();
    let waker = conn_io.waker.clone();
    let mut conn_lock = conn.lock().unwrap();

    debug!("after conn_lock");
    if !conn_lock.is_established() && !conn_lock.is_in_early_data() {
        debug!(
            "is_established{} in_early_data {} closed {}",
            conn_lock.is_established(),
            conn_lock.is_in_early_data(),
            conn_lock.is_closed()
        );
        return DtpError::DtpNotEstablished as c_int;
    }

    let mut ret = 0usize;

    let readable = conn_lock.readable();

    for s in readable {
        debug!("in dtp_api readable {}", s);
        match conn_lock.stream_recv(s, &mut buf) {
            Ok((read, finish)) => {
                debug!("{} received {} bytes", conn_lock.trace_id(), read);

                let stream_buf = &buf[..read];

                debug!(
                    "{} stream {} has {} bytes (fin? {})",
                    conn_lock.trace_id(),
                    s,
                    stream_buf.len(),
                    finish
                );
                *stream_id = s as c_int;
                *fin = finish;
                ret = read;
                break;
            }
            Err(Error::Done) => {
                continue;
            }
            Err(e) => {
                // TODO: 可能需要其他的处理
                ret = error_to_c(e) as usize;
                break;
            }
        }
    }

    // 唤醒 poll 并且发送数据
    // waker
    //     .lock()
    //     .unwrap()
    //     .wake()
    //     .expect(format!("failed to wake in dtp_send {}", conn_lock.trace_id()).as_str());

    return ret as c_int;
}
/*
发送数据
成功返回发送长度，失败返回-1。
这里的发送成功是指写入到了缓存中，后续会自动发送。
新增：streamid：选择使用哪个流进行传输，必须是0,4，8，16...(最低两位必须是0x00)(如果需要知道为什么这样，建议阅读rfc9000，第二章流的类型图)
并且！！！！在一个链接中，如果使用了标识fin==ture的一个流，则在同一会话中不允许再使用这个流id。不允许重复使用相同的流id！！！！！

fin：发送端使用，标识这个流的数据传输完成。同理接收端使用这个标识符来判断一个流是否结束。
*/
#[no_mangle]
pub extern "C" fn dtp_send(
    conn_io: *mut DtpConnection,
    buf: *const u8,
    buflen: c_int,
    fin: c_bool,
    stream_id: c_int,
) -> c_int {
    let conn_io = unsafe {
        if let Some(c) = conn_io.as_ref() {
            c
        } else {
            return DtpError::DtpNull as c_int;
        }
    };

    // 调用 api 发送数据
    let buf = unsafe { std::slice::from_raw_parts(buf, buflen as usize) };
    let conn = conn_io.conn.clone();
    let waker = conn_io.waker.clone();
    let mut conn_lock = conn.lock().unwrap();

    if !conn_lock.is_established() && !conn_lock.is_in_early_data() {
        info!(
            "dtp_send is_established {}, is_in_early_data {}, is_closed {}, is_timed_out {}",
            conn_lock.is_established(),
            conn_lock.is_in_early_data(),
            conn_lock.is_closed(),
            conn_lock.is_timed_out()
        );
        return DtpError::DtpNotEstablished as c_int;
    }

    let ret = match conn_lock.stream_send(stream_id as u64, buf, fin) {
        Ok(size) => size as c_int,
        Err(e) => error_to_c(e) as c_int,
    };

    // 唤醒 poll 并且发送数据
    waker
        .lock()
        .unwrap()
        .wake()
        .expect(format!("failed to wake in dtp_send {}", conn_lock.trace_id()).as_str());

    return ret;
}
// int dtp_send_with_priority(struct conn_io *conn_io, char *buf, int buflen, bool fin,
//                            int streamid, uint64_t priority);
/*关闭单个连接；用于客户端|服务端
成功返回1，失败返回-1
这个与下面dtp_close_connections的区别是，这个只会关闭单个链接
这个函数会 consume 掉 conn_io，之后不能再次使用指针调用 conn_io
*/
#[no_mangle]
pub extern "C" fn dtp_close(conn_io: *mut DtpConnection) -> c_int {
    let conn_io = if let Some(conn_io) = unsafe { conn_io.as_mut() } {
        conn_io
    } else {
        error!("error in dtp_close: conn_io is null");
        return -1;
    };

    if !conn_io.is_server_side {
        if conn_io.conn.lock().unwrap().is_established() {
            info!("send close in {:?}", conn_io.sockid);
            // 发送 close 信息
            conn_io
                .conn
                .lock()
                .unwrap()
                .close(true, 0, "".as_bytes())
                .expect("failed to send close");
            conn_io
                .waker
                .lock()
                .unwrap()
                .wake()
                .expect("failed to wake conn after sending close");
            // join client
            let client_handle = DTP_API_MAP
                .lock()
                .unwrap()
                .client_handles
                .remove(conn_io.sockid.as_ref().unwrap())
                .unwrap();
            client_handle.join().expect(
                format!(
                    "failed to join client_handle of {}",
                    conn_io.sockid.unwrap()
                )
                .as_str(),
            );
            // release resources
            // 我们可以确保与当前 client 有关的指针在这个函数结束的时候被 drop
            // client 的所有存在的指针只有三个
            // 第一个是 client_loop 对应的 handle，在刚刚已经 join 了。如果顺利的话。。。
            // 第二个是给出去的 Box::conn_io，里面包括了一个 conn 与一个 waker
            // 我们使用 Box::from_raw 将其所有权取回
            // 这个指针在这个 close 函数执行之后自动释放
            let _box_c = unsafe { Box::from_raw(conn_io) };
            // 最后是 client_loop 创建的 client 对象，其储存在 client_map 里面。
            // 因为其中包括所有 client 相关数据的指针，例如 poll, events，所以我们最后释放
            // let _client = DTP_API_MAP
            //     .lock()
            //     .unwrap()
            //     .client_map
            //     .remove(conn_io.sockid.as_ref().unwrap())
            //     .unwrap();
            return 1;
        } else {
            warn!("closing a not established connection {:?}", conn_io.sockid);
            return -1;
        }
    } else {
        if !conn_io.conn.lock().unwrap().is_closed() {
            debug!(
                "can't close connection: client not finished {}",
                conn_io.sockid.unwrap()
            );
            return -1;
        } else {
            // 这个分支只能用来释放 Box 中的信息
            // 理论上来说，我们完全就不需要处理 clients_lock 中的数据
            // 它一定能被 server_loop 因为超时回收
            // 我们只需要防止 conn_io_ptr 引出的数据不能被正常释放就可以

            // 这里可以看出，server 的 garbage collect 环节可能会使得
            // 某些 client 的资源被释放，但是 Box 中给出的 DtpConnection
            // 却在之后可以调用。
            // 为了防止这件事发生，我们将资源释放完全交给 server loop 处理
            // 这里只通过标记一个 status，允许 garbage collect 回收特定的 conn_io
            // TODO: 原来这么写会导致死锁，想想办法解决
            let conn_id = conn_io.conn_id.clone().unwrap();
            debug!("son of conections, id {:?}", conn_id);
            let pconns = conn_io.pconns.clone().unwrap();
            let mut shared_streams = pconns.lock().unwrap();
            if let Some((_, status)) = shared_streams.client_status.get_mut(&conn_id) {
                *status = ClientStatus::Close;
            }
            // 释放 conn_io
            let _conn_io = unsafe { Box::from_raw(conn_io) };

            return 1;
        }
    }
}

/*
关闭所有链接，只用于服务端.
成功返回1，失败返回-1.
这个和上一个的区别是，使用这个会关闭当前所有的链接，不管是否建立链接。
*/
#[no_mangle]
pub extern "C" fn dtp_close_connections(_conns: *mut DtpServerConns) -> c_int {
    return -1;
}

/*
用于服务端：
它返回以个pipe管道的读端。可以使用epoll等监听该文件描述符是否可读，如果判断为可读可调用dtp_recv获得数据。
成功返回>0,失败返回-1
TODO: 暂时禁用这个 API，使用 C 监听 Rust 的 raw_fd 可能产生不可预测的结果
*/
#[no_mangle]
pub extern "C" fn dtp_get_conns_listenfd(_conns: *mut DtpServerConns) -> c_int {
    return -1;
}

/*
用于服务端|客户端
获得一个链接的pipe管道读端，同理，可以使用epoll监听该文件描述符。
！！！这两个函数只能用于判断是否有数据可读，不可以使用于判断是否可写。！！
TODO: 暂时禁用这个 API，使用 C 监听 Rust 的 raw_fd 可能产生不可预测的结果
*/
#[no_mangle]
pub extern "C" fn dtp_get_connio_listenfd(_conn_io: *mut DtpConnection) -> c_int {
    return -1;
}

/*
用于判断当前的链接是否关闭
关闭了返回1，没有关闭返回 0，错误返回-1
*/
#[no_mangle]
pub extern "C" fn dtp_connect_is_close(conn_io: *mut DtpConnection) -> c_int {
    let conn_io = if let Some(s) = unsafe { conn_io.as_ref() } {
        s
    } else {
        return DtpError::DtpNull as c_int;
    };

    return conn_io.conn.lock().unwrap().is_closed() as c_int;
}

//--------参数配置------------------
// Configures the given certificate chain.
#[no_mangle]
pub extern "C" fn dtp_config_load_cert_chain_from_pem_file(
    config: *mut Config,
    path: *const c_char,
) -> c_int {
    if let Some(config) = unsafe { config.as_mut() } {
        let path = unsafe { ffi::CStr::from_ptr(path).to_str().unwrap() };
        return match config.load_cert_chain_from_pem_file(path) {
            Ok(_) => 0,

            Err(e) => error_to_c(e) as c_int,
        };
    } else {
        error!("some input args are null");
        return -1;
    }
}

// Configures the given private key.
#[no_mangle]
pub extern "C" fn dtp_config_load_priv_key_from_pem_file(
    config: *mut Config,
    path: *const c_char,
) -> c_int {
    if let Some(config) = unsafe { config.as_mut() } {
        let path = unsafe { ffi::CStr::from_ptr(path).to_str().unwrap() };
        return match config.load_priv_key_from_pem_file(path) {
            Ok(_) => 0,

            Err(e) => error_to_c(e) as c_int,
        };
    } else {
        error!("some input args are null");
        return -1;
    }
}

// Sets the `max_idle_timeout` transport parameter.
#[no_mangle]
pub extern "C" fn dtp_config_set_max_idle_timeout(config: *mut Config, v: u64) -> c_int {
    if let Some(config) = unsafe { config.as_mut() } {
        config.set_max_idle_timeout(v);
        return 0;
    } else {
        error!("some input args are null");
        return -1;
    }
}
// Sets the `initial_max_stream_data_bidi_local` transport parameter.
#[no_mangle]
pub extern "C" fn dtp_config_set_initial_max_stream_data_bidi_local(
    config: *mut Config,
    v: u64,
) -> c_int {
    if let Some(config) = unsafe { config.as_mut() } {
        config.set_initial_max_stream_data_bidi_local(v);
        return 0;
    } else {
        error!("some input args are null");
        return -1;
    }
}

// Sets the `initial_max_stream_data_bidi_remote` transport parameter.
#[no_mangle]
pub extern "C" fn dtp_config_set_initial_max_stream_data_bidi_remote(
    config: *mut Config,
    v: u64,
) -> c_int {
    if let Some(config) = unsafe { config.as_mut() } {
        config.set_initial_max_stream_data_bidi_remote(v);
        return 0;
    } else {
        error!("some input args are null");
        return -1;
    }
}

// Sets the `initial_max_stream_data_uni` transport parameter.
#[no_mangle]
pub extern "C" fn dtp_config_set_initial_max_stream_data_uni(config: *mut Config, v: u64) -> c_int {
    if let Some(config) = unsafe { config.as_mut() } {
        config.set_initial_max_stream_data_uni(v);
        return 0;
    } else {
        error!("some input args are null");
        return -1;
    }
}

// Sets the `initial_max_streams_bidi` transport parameter.
#[no_mangle]
pub extern "C" fn dtp_config_set_initial_max_streams_bidi(config: *mut Config, v: u64) -> c_int {
    if let Some(config) = unsafe { config.as_mut() } {
        config.set_initial_max_streams_bidi(v);
        return 0;
    } else {
        error!("some input args are null");
        return -1;
    }
}

// Sets the `initial_max_streams_uni` transport parameter.
#[no_mangle]
pub extern "C" fn dtp_config_set_initial_max_streams_uni(config: *mut Config, v: u64) -> c_int {
    if let Some(config) = unsafe { config.as_mut() } {
        config.set_initial_max_streams_uni(v);
        return 0;
    } else {
        error!("some input args are null");
        return -1;
    }
}

#[no_mangle]
//打开或者关闭国密
pub extern "C" fn dtp_config_set_gmssl_key(config: *mut Config, v: u64) -> c_int {
    if let Some(config) = unsafe { config.as_mut() } {
        config.set_gmssl(v);
        return 0;
    } else {
        error!("some input args are null");
        return -1;
    }
}

#[no_mangle]
pub extern "C" fn debug_init() {
    env_logger::init();
}