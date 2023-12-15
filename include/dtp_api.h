#ifndef _DTP_API_H_
#define _DTP_API_H_
#include "quiche.h"
// 只有服务端会使用的，管理 conn 的结构
// 结构体在 Rust 内部定义，不应进行直接访问
struct connections {
    void* data;
};

// 单个链接
// 结构体在 Rust 内部定义，不应进行直接访问
struct conn_io {
    void *data;
};

enum dtp_error {
    DTP_CLOSED = -22,
    DTP_NOT_ESTABLISHED = -42,
    DTP_NULL = -404,
    DTP_DEFAULT_ERR = -444
};

//----------API接口函数声明---------

/**
 * @brief 生成DTP的默认配置文件
 * @param {*}
 * @return {*}成功返回堆区的地址，失败返回NULL
 */
quiche_config *dtp_config_init();

/**
 * @brief 创建一个 dtp socket，并返回该sock
 * @attention
 * !! 这个 sock 并不是一个真实 fd，而是一个 Rust 库中的标记
 * !! 实际的 fd 在 Rust 代码中，目前不能获得
 * @param {*}
 * @return {*} 成功返回sock，失败返回-1
 */
int dtp_socket();

/**
 * @brief 在调用 dtp_socket 之后发生其他的意外时进行调用，关闭 dtp_socket
 * @attention
 * !! 这个函数的调用必须保证对应的 socket 没有成功生成 server 或 client
 * @param sockfd 
 * @return int 
 */
int dtp_socket_close(int sockfd);

/**
 * @brief 绑定本地的sock，使用方法类似tcp的bind
 * @attention 这个函数只有在启动 server 之前需要调用
 * @param {*}
 * @return {*}成功返回1，失败返回-1
 */
int dtp_bind(int sockfd, const char *ip, int port);

/**
 * @brief 监听函数 创建一个DTP server 线程，并返回一个conns结构体。
 * 
 * @param conn_io *conns
 * @return {*}
 */
struct connections *dtp_listen(int sock, quiche_config *config);

/**
 * @brief 接受链接
 * @attention 这个函数只有在启动了 DTP server 之后才能使用
 * @param is_block 是否阻塞；false = 非阻塞
 * @return {*} 成功返回一个地址，失败返回NULL
*/
struct conn_io *dtp_accept(struct connections *conns, bool is_block);

/**
 * @brief 发起对服务器的连接
 * @return 成功返回地址，失败返回NULL
*/
struct conn_io *dtp_connect(int sock, const char *ip, int port, quiche_config *config);

/**
 * @brief 接收数据，这个函数是非阻塞的
 * @details 新增 uin64_t streamid 和bool fin；需要传入指针。streamid是标识收到的数据是从哪个流中获得。fin表示这个流是否关闭
 * @return 成功返回接受的长度，失败返回-1
 * 当返回-1的时候，建议对链接判断是否关闭。
 * 
*/
int dtp_recv(struct conn_io *conn_io, char *buf, int buflen, uint64_t *streamid, bool *fin);

/**
 * @brief 发送数据
 * @param fin 发送端使用，标识这个流的数据传输完成。同理接收端使用这个标识符来判断一个流是否结束。
 * @return 这里的发送成功是指写入到了缓存中，后续会自动发送。
 * @details 新增：streamid：选择使用哪个流进行传输，必须是0,4，8，16...(最低两位必须是0x00)(如果需要知道为什么这样，建议阅读rfc9000，第二章流的类型图)
 * 并且！！！！在一个链接中，如果使用了标识fin==ture的一个流，则在同一会话中不允许再使用这个流id。不允许重复使用相同的流id！！！！！
 */
int dtp_send(struct conn_io *conn_io, char *buf, int buflen, bool fin,
             int streamid);

/**
 * @brief 关闭单个连接：用于客户端|服务端
 * @details 这个函数与dtp_close_connections的区别是，这个只会关闭单个链接
 * @return 成功返回 1，失败返回 -1
*/
int dtp_close(struct conn_io *conn_io);

/*

*/
/**
 * @brief 关闭所有链接，只用于服务端
 * @details 这个函数和 dtp_close 的区别是，使用这个会关闭当前所有的链接，不管是否建立链接。
 * @return 成功返回1，失败返回-1.
*/
int dtp_close_connections(struct connections *conns);

/**
 * @brief 
 * @deprecated 该函数目前被禁用
 */
// int dtp_get_conns_listenfd(struct connections *conns);

/**
 * @brief
 * @deprecated 该函数目前被禁用
*/
// int dtp_get_connio_listenfd(struct conn_io *conn_io);

/**
 * @brief 用于判断当前的链接是否关闭
 * @return 关闭了返回1.错误返回-1
*/
int dtp_connect_is_close(struct conn_io *conn_io);

//--------参数配置------------------

// Configures the given certificate chain.
int dtp_config_load_cert_chain_from_pem_file(quiche_config *config, const char *path);

// Configures the given private key.
int dtp_config_load_priv_key_from_pem_file(quiche_config *config, const char *path);

// Sets the `max_idle_timeout` transport parameter.
int dtp_config_set_max_idle_timeout(quiche_config *config, uint64_t v);

// Sets the `initial_max_stream_data_bidi_local` transport parameter.
int dtp_config_set_initial_max_stream_data_bidi_local(quiche_config *config, uint64_t v);

// Sets the `initial_max_stream_data_bidi_remote` transport parameter.
int dtp_config_set_initial_max_stream_data_bidi_remote(quiche_config *config, uint64_t v);

// Sets the `initial_max_stream_data_uni` transport parameter.
int dtp_config_set_initial_max_stream_data_uni(quiche_config *config, uint64_t v);

// Sets the `initial_max_streams_bidi` transport parameter.
int dtp_config_set_initial_max_streams_bidi(quiche_config *config, uint64_t v);

// Sets the `initial_max_streams_uni` transport parameter.
int dtp_config_set_initial_max_streams_uni(quiche_config *config, uint64_t v);

//打开或者关闭国密
int dtp_config_set_gmssl_key( quiche_config * config, uint64_t v);

// Debug 使用
void debug_init();

#endif // _DTP_API_H_