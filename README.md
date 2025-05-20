# Rust WebSocket 示例

这个项目包含一个简单的WebSocket服务器和客户端实现，使用Rust语言编写。

## 功能

- WebSocket服务器：监听9001端口，处理客户端连接
- WebSocket客户端：可以连接到服务器并发送/接收消息
- 支持文本消息的发送和接收
- 自动处理ping/pong消息
- 优雅处理连接关闭

## 使用方法

### 编译项目

```bash
cargo build --release
```

### 运行服务器

```bash
cargo run --bin server
```

或者直接运行编译后的二进制文件：

```bash
./target/release/server
```

服务器将启动并监听127.0.0.1:9001端口。

### 运行客户端

```bash
cargo run --bin client
```

或者直接运行编译后的二进制文件：

```bash
./target/release/client
```

客户端将连接到本地服务器，连接成功后，您可以输入消息并按回车键发送。输入`exit`可以退出客户端。

## 注意事项

- 必须先启动服务器，再启动客户端
- 客户端默认连接到127.0.0.1:9001
- 可以同时运行多个客户端连接到同一个服务器 