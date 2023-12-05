use std::error;
use std::ffi;
use libc::c_char;
use libc::c_int;
use libc::c_void;
use libc::size_t;
use libc::sockaddr;
use libc::ssize_t;
use libc::timespec;

use quiche::Config;
use quiche::Error;

type c_bool = bool;

#[repr(C)]
#[derive(Default, Debug, Copy, Clone)]
pub struct DtpConnection {

}

#[repr(C)]
#[derive(Default, Debug, Copy, Clone)]
pub struct DtpServer {

}

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

/*
创建套接字
创建一个非阻塞的，地址可复用的udp；和tcp中的bind无异
成功返回>0的sock，失败返回-1
*/
#[no_mangle]
pub extern fn dtp_socket() -> c_int {
    return -1;
}

/**
 * @brief 在调用 dtp_socket 之后发生其他的意外时进行调用，关闭 dtp_socket
 * 
 * @param sock 
 * @return int =0 成功，其他都是失败
 */
#[no_mangle]
pub extern fn dtp_socket_close(sock: c_int) -> c_int {
    return -1;
}

/*
绑定端口
给套接字绑定一个地址；和tcp中的bind无异
成功返回1，失败返回-1
*/
#[no_mangle]
pub extern fn dtp_bind(sock: c_int, ip: *const c_char, port: c_int) 
->c_int {
    return -1;
}

/*
监听端口
设置监听端口
非阻塞
成功返回一个会话地址，失败返回NULL
*/
#[no_mangle]
pub extern fn dtp_listen(sock: c_int, config :*mut Config)
->*mut DtpServer {
    return Box::into_raw(Box::new(DtpServer::default()));
}

/*
接受链接
is_block：是否阻塞；false = 非阻塞
成功返回一个地址，失败返回NULL
*/
#[no_mangle]
pub extern fn dtp_accept(sock: c_int, config :*mut Config, is_block: c_bool)
->*mut DtpConnection {
    return Box::into_raw(Box::new(DtpConnection::default()));
}

/*
发起连接
成功返回地址，失败返回NULL
*/
#[no_mangle]
pub extern fn dtp_connect(sock: c_int, ip: *const c_char, port: c_int, config :*mut Config)
->*mut DtpConnection {
    return Box::into_raw(Box::new(DtpConnection::default()));
}

/*
接受数据，这个函数是非阻塞的
成功返回接受的长度，失败返回-1
新增 uin64_t streamid 和bool fin；需要传入指针。streamid是标识收到的数据是从哪个流中获得。fin表示这个流是否关闭
当返回-1的时候，建议对链接判断是否关闭。
*/
#[no_mangle]
pub extern fn dtp_recv(
    conns: *mut DtpConnection, 
    buf: *mut u8, buflen: c_int, 
    stream_id: *mut c_int, 
    fin: *mut c_bool)
-> c_int {
    return -1;
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
pub extern fn dtp_send(conns: *mut DtpConnection, 
    buf: *const u8, buflen: c_int, 
    fin: c_bool, stream_id: c_int)
-> c_int {
    return -1;
}
// int dtp_send_with_priority(struct conn_io *conn_io, char *buf, int buflen, bool fin,
//                            int streamid, uint64_t priority);
/*关闭单个连接；用于客户端|服务端
成功返回1，失败返回-1
这个与下面dtp_close_connections的区别是，这个只会关闭单个链接
*/
#[no_mangle]
pub extern fn dtp_close(conns: *mut DtpConnection)
-> c_int {
    return -1;
}

/*
关闭所有链接，只用于服务端.
成功返回1，失败返回-1.
这个和上一个的区别是，使用这个会关闭当前所有的链接，不管是否建立链接。
*/
#[no_mangle]
pub extern fn dtp_close_connections(conns: *mut DtpServer)
-> c_int {
    return -1;
}

/*
用于服务端：
它返回以个pipe管道的读端。可以使用epoll等监听该文件描述符是否可读，如果判断为可读可调用dtp_recv获得数据。
成功返回>0,失败返回-1
*/
#[no_mangle]
pub extern fn dtp_get_conns_listenfd(conns: *mut DtpServer)
-> c_int {
    return -1;
}

/*
用于服务端|客户端
获得一个链接的pipe管道读端，同理，可以使用epoll监听该文件描述符。
！！！这两个函数只能用于判断是否有数据可读，不可以使用于判断是否可写。！！
*/
#[no_mangle]
pub extern fn dtp_get_connio_listenfd(conn_io: *mut DtpConnection)
-> c_int {
    return -1;
}

/*
用于判断当前的链接是否关闭
关闭了返回1.错误返回-1
*/
#[no_mangle]
pub extern fn dtp_connect_is_close(conn_io: *mut DtpConnection)
-> c_int {
    return -1;
}

//--------参数配置------------------
// Configures the given certificate chain.
#[no_mangle]
pub extern fn dtp_config_load_cert_chain_from_pem_file(config: *mut Config, path: *const c_char)
-> c_int {
    if config.is_null() {
        eprintln!("some input args are null");
        return -1;
    }
    let path = unsafe { ffi::CStr::from_ptr(path).to_str().unwrap() };

    unsafe {
        let config = &mut *config;

        return match config.load_cert_chain_from_pem_file(path) {
            Ok(_) => 0,

            Err(e) => error_to_c(e) as c_int,
        }
    }
}

// Configures the given private key.
#[no_mangle]
pub extern fn dtp_config_load_priv_key_from_pem_file(config: *mut Config, path: *const c_char)
-> c_int {
    if config.is_null() {
        eprintln!("some input args are null");
        return -1;
    }
    let path = unsafe { ffi::CStr::from_ptr(path).to_str().unwrap() };

    unsafe { 
        let config = &mut *config;
        return match config.load_priv_key_from_pem_file(path) {
            Ok(_) => 0,

            Err(e) => error_to_c(e) as c_int,
        }
    };
}

// Sets the `max_idle_timeout` transport parameter.
#[no_mangle]
pub extern fn dtp_config_set_max_idle_timeout(config: *mut Config, v: u64)
-> c_int {
    if config.is_null() {
        eprintln!("some input args are null");
        return -1;
    }
    unsafe { 
        let config = &mut *config;
        config.set_max_idle_timeout(v);
        return 0;
    };
}
// Sets the `initial_max_stream_data_bidi_local` transport parameter.
#[no_mangle]
pub extern fn dtp_config_set_initial_max_stream_data_bidi_local(config: *mut Config, v: u64)
-> c_int {
    if config.is_null() {
        eprintln!("some input args are null");
        return -1;
    }
    unsafe { 
        let config = &mut *config;
        config.set_initial_max_stream_data_bidi_local(v);
        return 0;
    };
}

// Sets the `initial_max_stream_data_bidi_remote` transport parameter.
#[no_mangle]
pub extern fn dtp_config_set_initial_max_stream_data_bidi_remote(config: *mut Config, v: u64)
-> c_int {
    if config.is_null() {
        eprintln!("some input args are null");
        return -1;
    }

    unsafe { 
        let config = &mut *config;
        config.set_initial_max_stream_data_bidi_remote(v);
        return 0;
    };
}

// Sets the `initial_max_stream_data_uni` transport parameter.
#[no_mangle]
pub extern fn dtp_config_set_initial_max_stream_data_uni(config: *mut Config, v: u64)
-> c_int {
    if config.is_null() {
        eprintln!("some input args are null");
        return -1;
    }
    unsafe { 
        let config = &mut *config;
        config.set_initial_max_stream_data_uni(v);
        return 0;
    };
}

// Sets the `initial_max_streams_bidi` transport parameter.
#[no_mangle]
pub extern fn dtp_config_set_initial_max_streams_bidi(config: *mut Config, v: u64)
-> c_int {
    if config.is_null() {
        eprintln!("some input args are null");
        return -1;
    }
    unsafe { 
        let config = &mut *config;
        config.set_initial_max_streams_bidi(v);
        return 0;
    };
}

// Sets the `initial_max_streams_uni` transport parameter.
#[no_mangle]
pub extern fn dtp_config_set_initial_max_streams_uni(config: *mut Config, v: u64) 
-> c_int {
    if config.is_null() {
        eprintln!("some input args are null");
        return -1;
    }
    unsafe { 
        let config = &mut *config;
        config.set_initial_max_streams_uni(v);
        return 0;
    };
}

#[no_mangle]
//打开或者关闭国密
pub extern fn dtp_config_set_gmssl_key(config: *mut Config, v: u64) 
-> c_int {
    if config.is_null() {
        return -1;
    }
    unsafe { 
        let config = &mut *config;
        config.set_gmssl(v);
        return 0;
    };
}