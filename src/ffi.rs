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

/*
创建套接字
创建一个非阻塞的，地址可复用的udp；和tcp中的bind无异
成功返回>0的sock，失败返回-1
*/
#[no_mangle]
pub extern "C" fn dtp_socket() -> c_int {
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
    if api_map.sock_map.get(&sock).is_none() {
        error!("failed to close dtp_socket: no such sock, {}", sock);
        return -1;
    }
    if api_map.client_map.get(&sock).is_some() {
        error!("failed to close dtp_socket: sock in use (client), {}", sock);
        return -1;
    }
    if api_map.server_map.get(&sock).is_some() {
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

    api_map.sock_map.insert(sock, Some(server.socket.clone()));
    api_map
        .server_map
        .insert(sock, Arc::new(Mutex::new(server)));
    return 1;
}

/*
监听端口
设置监听端口
非阻塞
成功返回一个会话地址，失败返回NULL
只有 server 会调用这个函数
dtp_listen 会 consume 掉 config 。它不需要再进行释放，同时也不能再次被使用。
*/
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
    let server_arc = {
        if let Some(s) = api_map.server_map.get_mut(&sock) {
            let config = unsafe { Box::from_raw(config) };
            let config = Arc::new(Mutex::new(*config));
            s.lock().unwrap().config = Some(config);
            s.clone()
        } else {
            error!("error in dtp_listen: sock is not binded, {}", sock);
            return std::ptr::null_mut();
        }
    };

    let server_clone = server_arc.clone();
    let waker = server_arc.clone().lock().unwrap().waker.clone();
    // TODO: 使用线程来运行 DtpServer 以及相关的程序
    let h = std::thread::spawn(move || {
        let (clients, p, e, s, c, w) = {
            let server = server_clone.lock().unwrap();
            let clients = server.clients.clone();
            let p = server.poll.clone();
            let e = server.events.clone();
            let s = server.socket.clone();
            let c = server.config.clone().unwrap().clone();
            let w = server.waker.clone();
            (clients, p, e, s, c, w)
        };
        server_loop(clients, p, e, s, c, w).unwrap();
    });
    api_map.server_handles.insert(sock, h);

    let conns = DtpServerConns {
        server: server_arc.clone(),
        waker,
        sockid: sock,
    };

    return Box::into_raw(Box::new(conns));
}

/*
接受链接
is_block：是否阻塞；false = 非阻塞
成功返回一个地址，失败返回NULL
只有 server 会调用这个函数
*/
#[no_mangle]
pub extern "C" fn dtp_accept(
    _sock: c_int,
    _config: *mut Config,
    _is_block: c_bool,
) -> *mut DtpConnection {
    return std::ptr::null_mut();
}

/*
发起连接
成功返回地址，失败返回NULL
只有 client 会调用这个函数
*/
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
    // 创建 client
    let client = DtpClient::connect(ip.to_owned(), port, config, sock).unwrap();
    // move client 到一个储存的地方
    let client_arc = Arc::new(Mutex::new(client));

    let client_arc_clone = client_arc.clone();

    let client_clone = {
        let client_lock = client_arc.lock().unwrap();
        client_lock.clone()
    };

    let conn = client_clone.conn.clone();
    let conn_clone = conn.clone();
    let waker = client_clone.waker.clone();
    let waker_clone = waker.clone();
    let socket = client_clone.socket.clone();

    api_map.sock_map.insert(sock, Some(socket));

    let (p, e, c, s, peer) = {
        let client = client_arc_clone.lock().unwrap().clone();
        (
            client.poll,
            client.events,
            client.conn,
            client.socket,
            client.peer_addr,
        )
    };

    api_map.sock_map.get_mut(&sock).as_mut();
    api_map.client_map.insert(sock, client_arc.clone());

    // TODO: 运行 client 的线程
    let h = std::thread::spawn(move || {
        client_loop(p, e, c, s, peer, sock).unwrap();
        info!("client main loop stopped {}", sock);
    });

    // TODO: 运行通讯线程
    // let (tx, rx) = tokio::sync::mpsc::channel(32);

    // let hm = tokio::spawn(async move {
    //     recv_msg_loop(rx, conn, waker).await;
    //     println!("client msg loop finished {}", sock);
    // });

    api_map.client_handles.insert(sock, h);

    let conn_io = DtpConnection {
        // client: Some(client_arc.clone()),
        conn: conn_clone,
        waker: waker_clone,
        // handle: Some(h),
        // msg_handle: Some(hm),
        is_server_side: false,
        // tx: Some(tx),
        sockid: sock,
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

    if !conn_lock.is_established() && !conn_lock.is_in_early_data() {
        return DtpError::DtpNotEstablished as c_int;
    }

    let mut ret = 0usize;

    let readable = conn_lock.readable();

    for s in readable {
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
    waker
        .lock()
        .unwrap()
        .wake()
        .expect(format!("failed to wake in dtp_send {}", conn_lock.trace_id()).as_str());

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

    if !conn_io.is_server_side && conn_io.conn.lock().unwrap().is_established() {
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
    } else if conn_io.is_server_side && conn_io.conn.lock().unwrap().is_closed() {
        debug!(
            "can't close connection: client not finished {}",
            conn_io.sockid
        );
        return -1;
    } else {
        // debug!("son of conections,pconns %p", conn_io->pconns);
        // pthread_rwlock_rdlock(&(conn_io->pconns->rwlock_hash));
        // HASH_DELETE(hh, conn_io->pconns->h, conn_io);
        // pthread_rwlock_unlock(&conn_io->pconns->rwlock_hash);
    }

    if !conn_io.is_server_side {
        // join client
        let client_handle = DTP_API_MAP
            .lock()
            .unwrap()
            .client_handles
            .remove(&conn_io.sockid)
            .unwrap();
        client_handle
            .join()
            .expect(format!("failed to join client_handle of {}", conn_io.sockid).as_str());
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
        let _client = DTP_API_MAP
            .lock()
            .unwrap()
            .client_map
            .remove(&conn_io.sockid)
            .unwrap();
    } else {
        // remove conn_io from hash_table at server_side
        // release resources
    }

    return 1;
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
TODO: 暂时禁用这个 API
*/
#[no_mangle]
pub extern "C" fn dtp_get_conns_listenfd(_conns: *mut DtpServerConns) -> c_int {
    return -1;
}

/*
用于服务端|客户端
获得一个链接的pipe管道读端，同理，可以使用epoll监听该文件描述符。
！！！这两个函数只能用于判断是否有数据可读，不可以使用于判断是否可写。！！
TODO: 暂时禁用这个 API
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
    let api_map = DTP_API_MAP.lock().unwrap();
    if let Some(client) = api_map.client_map.get(&conn_io.sockid) {
        let conn = client.lock().unwrap().conn.clone();
        return conn.lock().unwrap().is_closed() as c_int;
    } else {
        error!(
            "error in dtp_connect_is_close: no such conn from sock {}",
            conn_io.sockid
        );
        return DtpError::DtpDefaultErr as c_int;
    }
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
