use std::io::{self, Write};
use std::time::Duration;
use clap::{Arg, Command};
use std::collections::HashMap;
use url::Url;

// 引入lazy_static
#[macro_use]
extern crate lazy_static;

// 定义跳转宏，用于代码流程控制
macro_rules! goto_default_connection {
    () => {
        // 不做任何事，只作为标记
        // 这里的逻辑已经改为直接在代码中继续执行默认连接流程
    };
}

// tokio相关依赖
use tokio::net::TcpStream;
use tokio::runtime::Runtime;
use tokio::io::{AsyncReadExt, AsyncWriteExt, AsyncRead, AsyncWrite};
use tokio_tungstenite::{connect_async};
use futures_util::{SinkExt, StreamExt};
use tungstenite::Message;
use tokio_native_tls::native_tls;
use tungstenite::client::IntoClientRequest;

// 添加引入
use native_tls::TlsConnector;
use tokio_native_tls::TlsConnector as TokioTlsConnector;

// 定义一个同时包含AsyncRead和AsyncWrite的新特征
trait AsyncStream: AsyncRead + AsyncWrite + Unpin + Send {}

// 为所有同时实现AsyncRead、AsyncWrite、Unpin和Send的类型实现AsyncStream
impl<T: AsyncRead + AsyncWrite + Unpin + Send> AsyncStream for T {}

// 记录用于标识不同代理类型
enum ProxyType {
    None,
    Http(String),
    Socks5(String),
}

// 配置结构体
struct Config {
    server_url: String,       // WebSocket服务器地址
    use_proxy: bool,          // 是否使用代理
    proxy_url: Option<String>, // 代理服务器地址
    proxy_type: ProxyType,    // 代理类型
}

// WebSocket服务器信息
struct BinanceWSInfo {
    base_url: &'static str,
    description: &'static str,
}

// 预设的WebSocket服务器列表
lazy_static! {
    static ref WS_SERVERS: HashMap<&'static str, BinanceWSInfo> = {
        let mut m = HashMap::new();
        m.insert("binance", BinanceWSInfo {
            base_url: "wss://fstream.binance.com",
            description: "币安期货WebSocket (需要代理)"
        });
        m.insert("binance_spot", BinanceWSInfo {
            base_url: "wss://stream.binance.com:9443",
            description: "币安现货WebSocket (需要代理)"
        });
        m.insert("echo", BinanceWSInfo {
            base_url: "wss://echo.websocket.org",
            description: "Echo测试服务器"
        });
        m.insert("piesocket", BinanceWSInfo {
            base_url: "wss://demo.piesocket.com/v3/channel_1?api_key=VCXCEuvhGcBDP7XhiJJUDvR1e1D3eiVjgZ9VRiaV",
            description: "PieSocket测试服务器"
        });
        m.insert("local", BinanceWSInfo {
            base_url: "ws://localhost:8080",
            description: "本地测试服务器"
        });
        m
    };
}

// 通过代理连接到目标服务器
async fn connect_via_proxy(proxy_url_str: &str, target_host: &str, target_port: u16) -> Result<TcpStream, Box<dyn std::error::Error>> {
    println!("使用代理: {}", proxy_url_str);
    
    // 解析代理URL
    let proxy_url = Url::parse(proxy_url_str)?;
    let proxy_host = proxy_url.host_str().ok_or("无法解析代理主机名")?;
    let proxy_port = proxy_url.port().unwrap_or(if proxy_url.scheme() == "socks5" { 1080 } else { 7890 });
    
    println!("代理服务器: {}:{}", proxy_host, proxy_port);
    
    // 判断代理类型
    if proxy_url.scheme() == "socks5" {
        println!("使用SOCKS5代理");
        
        use tokio_socks::tcp::Socks5Stream;
        
        // 连接SOCKS5代理服务器，并创建到目标服务器的连接
        println!("通过SOCKS5代理连接到 {}:{}", target_host, target_port);
        
        let socket = match Socks5Stream::connect(
            format!("{}:{}", proxy_host, proxy_port).as_str(),
            format!("{}:{}", target_host, target_port).as_str(),
        ).await {
            Ok(socket) => {
                println!("SOCKS5代理连接成功！");
                socket.into_inner()
            },
            Err(e) => {
                println!("SOCKS5代理连接失败: {}", e);
                return Err(format!("SOCKS5代理连接失败: {}", e).into());
            }
        };
        
        Ok(socket)
    } else if proxy_url.scheme() == "http" || proxy_url.scheme() == "https" {
        println!("使用HTTP(S)代理，发送CONNECT请求");
        
        // 连接到代理服务器
        println!("正在连接到代理服务器...");
        let proxy_addr = format!("{}:{}", proxy_host, proxy_port);
        let mut tcp_stream = TcpStream::connect(proxy_addr).await?;
        
        // 目标地址
        let target_addr = format!("{}:{}", target_host, target_port);
        
        // 构建CONNECT请求
        let connect_req = format!(
            "CONNECT {} HTTP/1.1\r\n\
            Host: {}\r\n\
            User-Agent: rustws-client\r\n\
            Connection: Keep-Alive\r\n\
            Proxy-Connection: Keep-Alive\r\n\r\n",
            target_addr, target_host
        );
        
        // 发送CONNECT请求
        tcp_stream.write_all(connect_req.as_bytes()).await?;
        
        // 读取代理服务器的响应
        let mut response_buf = [0; 1024];
        let n = tcp_stream.read(&mut response_buf).await?;
        let response_text = String::from_utf8_lossy(&response_buf[..n]);
        println!("代理响应: {}", response_text);
        
        // 检查代理服务器是否同意连接
        if !response_text.contains("200") {
            return Err(format!("代理服务器拒绝连接: {}", response_text).into());
        }
        
        println!("CONNECT请求成功，代理服务器已建立连接到目标服务器");
        Ok(tcp_stream)
    } else {
        return Err(format!("不支持的代理协议: {}", proxy_url.scheme()).into());
    }
}

// 连接到WebSocket服务器
async fn connect_to_server(config: &Config, timeout_secs: u64) -> Result<(), Box<dyn std::error::Error>> {
    let url = Url::parse(&config.server_url)?;
    println!("连接到WebSocket服务器: {}", url);
    
    // 解析主机和端口，用于代理连接
    let host = url.host_str().ok_or("无法解析主机名")?;
    let port = url.port().unwrap_or(if url.scheme() == "wss" { 443 } else { 80 });
    
    // 当使用wss协议时，确保使用TLS
    let tls_connector = if url.scheme() == "wss" {
        println!("使用TLS连接...");
        Some(native_tls::TlsConnector::builder()
            .danger_accept_invalid_certs(true) // 接受无效证书，适用于测试环境
            .danger_accept_invalid_hostnames(true) // 接受无效主机名，适用于测试环境
            .build()?)
    } else {
        None
    };
    
    // 处理代理连接
    if config.use_proxy {
        if let Some(proxy_url_str) = &config.proxy_url {
            println!("使用代理: {}", proxy_url_str);
            
            println!("尝试直接通过代理连接...");
            // 1. 尝试直接通过代理连接到WebSocket服务器
            let tcp_stream = match tokio::time::timeout(
                Duration::from_secs(timeout_secs/2), // 代理连接使用一半的超时时间
                connect_via_proxy(proxy_url_str, host, port)
            ).await {
                Ok(result) => match result {
                    Ok(stream) => {
                        println!("通过代理建立到服务器的TCP连接成功!");
                        stream
                    },
                    Err(e) => {
                        println!("通过代理连接失败: {}", e);
                        println!("尝试使用环境变量方式...");
                        
                        // 设置代理环境变量 (备选方案)
                        unsafe {
                            std::env::set_var("HTTP_PROXY", proxy_url_str);
                            std::env::set_var("HTTPS_PROXY", proxy_url_str);
                            std::env::set_var("WSS_PROXY", proxy_url_str); 
                            std::env::set_var("NO_PROXY", "localhost,127.0.0.1,::1");
                        }
                        println!("已设置代理环境变量，尝试连接...");
                        
                        // 使用默认方式连接
                        return connect_with_env_proxy(url, config, timeout_secs, tls_connector).await;
                    }
                },
                Err(_) => {
                    println!("通过代理连接超时，尝试环境变量方式...");
                    
                    // 设置代理环境变量 (备选方案)
                    unsafe {
                        std::env::set_var("HTTP_PROXY", proxy_url_str);
                        std::env::set_var("HTTPS_PROXY", proxy_url_str);
                        std::env::set_var("WSS_PROXY", proxy_url_str);
                        std::env::set_var("NO_PROXY", "localhost,127.0.0.1,::1");
                    }
                    println!("已设置代理环境变量，尝试连接...");
                    
                    // 使用默认方式连接
                    return connect_with_env_proxy(url, config, timeout_secs, tls_connector).await;
                }
            };
            
            // 2. 如果需要TLS，将TCP流包装成TLS流
            let stream: Box<dyn AsyncStream> = 
                if let Some(tls) = tls_connector {
                    println!("通过TLS加密连接...");
                    match tokio_native_tls::TlsConnector::from(tls).connect(host, tcp_stream).await {
                        Ok(tls_stream) => {
                            println!("TLS连接成功!");
                            Box::new(tls_stream)
                        },
                        Err(e) => {
                            println!("TLS连接失败: {}", e);
                            return Err(e.into());
                        }
                    }
                } else {
                    println!("不使用TLS加密...");
                    Box::new(tcp_stream)
                };
            
            // 3. 建立WebSocket连接
            println!("升级到WebSocket协议...");
            use tokio_tungstenite::WebSocketStream;
            
            let mut request = url.into_client_request()?;
            let headers = request.headers_mut();
            headers.insert("User-Agent", "RustWebSocketClient/1.0".parse().unwrap());
            
            println!("准备WebSocket请求: {:?}", request);
            
            let (ws_stream, response) = match tokio::time::timeout(
                Duration::from_secs(timeout_secs), 
                tokio_tungstenite::client_async_with_config(
                    request, 
                    stream,
                    None
                )
            ).await {
                Ok(result) => match result {
                    Ok((ws, response)) => {
                        println!("WebSocket连接成功!");
                        (ws, response)
                    },
                    Err(e) => {
                        println!("WebSocket连接失败: {}", e);
                        return Err(e.into());
                    }
                },
                Err(_) => {
                    println!("WebSocket连接超时!");
                    return Err("WebSocket连接超时".into());
                }
            };
            
            println!("WebSocket连接成功！");
            println!("服务器响应状态: {}", response.status());
            
            // 分离发送者和接收者
            let (mut ws_sender, mut ws_receiver) = ws_stream.split();
            
            // 包装发送者以便在多个任务间共享
            use std::sync::Arc;
            use tokio::sync::Mutex;
            let ws_sender = Arc::new(Mutex::new(ws_sender));
            let ws_sender_clone = ws_sender.clone();
            
            // 发送几条测试消息
            for i in 1..2 {  // 只发送一条，避免被断开连接
                // 不再发送币安不支持的PING方法，而是直接发送websocket原生的ping帧
                println!("发送WebSocket ping帧...");
                ws_sender.lock().await.send(Message::Ping(vec![])).await?;
                println!("ping帧发送成功!");
                
                // 等待服务器响应
                if let Some(result) = ws_receiver.next().await {
                    match result {
                        Ok(msg) => {
                            match msg {
                                Message::Text(text) => {
                                    println!("收到服务器响应: {}", text);
                                },
                                Message::Binary(data) => println!("收到二进制数据, 长度: {} 字节", data.len()),
                                Message::Pong(_) => println!("收到服务器的Pong响应"),
                                Message::Close(_) => {
                                    println!("服务器关闭了连接");
                                    return Ok(());
                                },
                                _ => println!("收到其他类型消息: {:?}", msg),
                            }
                        },
                        Err(e) => {
                            println!("接收消息错误: {:?}", e);
                            return Err(e.into());
                        }
                    }
                } else {
                    println!("WebSocket连接关闭");
                    return Ok(());
                }
                
                tokio::time::sleep(Duration::from_secs(1)).await;
            }
            
            // 发送订阅请求
            let sub_msg = r#"{"method":"SUBSCRIBE","params":["bnbusdt@aggTrade"],"id":1}"#;
            println!("发送订阅请求: {}", sub_msg);
            ws_sender.lock().await.send(Message::Text(sub_msg.to_string())).await?;
            
            // 等待订阅响应
            if let Some(result) = ws_receiver.next().await {
                match result {
                    Ok(msg) => {
                        match msg {
                            Message::Text(text) => println!("收到订阅响应: {}", text),
                            _ => println!("收到意外响应类型: {:?}", msg),
                        }
                    },
                    Err(e) => {
                        println!("接收订阅响应错误: {:?}", e);
                        return Err(e.into());
                    }
                }
            }

            // 启动一个异步任务来接收消息并自动响应ping
            let _receive_task = tokio::spawn(async move {
                // 记录上次发送pong的时间
                let mut last_pong_time = std::time::Instant::now();
                
                while let Some(result) = ws_receiver.next().await {
                    match result {
                        Ok(msg) => {
                            match msg {
                                Message::Text(text) => println!("收到消息: {}", text),
                                Message::Binary(data) => println!("收到二进制数据, 长度: {} 字节", data.len()),
                                Message::Ping(data) => {
                                    println!("收到Ping，自动响应Pong");
                                    // 自动响应pong，币安要求在10分钟内回复
                                    if let Err(e) = ws_sender_clone.lock().await.send(Message::Pong(data)).await {
                                        println!("发送Pong失败: {:?}", e);
                                        break;
                                    }
                                    last_pong_time = std::time::Instant::now();
                                },
                                Message::Pong(_) => {
                                    println!("收到Pong响应");
                                    last_pong_time = std::time::Instant::now();
                                },
                                Message::Close(_) => {
                                    println!("服务器关闭了连接");
                                    break;
                                },
                                _ => println!("收到其他类型消息: {:?}", msg),
                            }
                            
                            // 每3分钟主动发送一次pong，保持连接
                            // 币安服务器每3分钟发送ping，我们主动每3分钟发送pong以确保连接不会断开
                            if last_pong_time.elapsed() > Duration::from_secs(180) {
                                println!("发送保活Pong（币安连接要求）");
                                if let Err(e) = ws_sender_clone.lock().await.send(Message::Pong(vec![])).await {
                                    println!("发送保活Pong失败: {:?}", e);
                                    break;
                                }
                                last_pong_time = std::time::Instant::now();
                            }
                        },
                        Err(e) => {
                            println!("接收消息错误: {:?}", e);
                            break;
                        }
                    }
                }
                
                println!("接收任务结束");
            });
            
            // 主循环处理用户输入
            loop {
                // 提示用户输入
                print!("> ");
                io::stdout().flush().unwrap();
                
                let mut input = String::new();
                io::stdin().read_line(&mut input).unwrap();
                
                // 去除尾部换行符
                let input = input.trim();
                if input == "exit" {
                    // 发送关闭消息
                    println!("退出命令，准备关闭连接");
                    if let Err(e) = ws_sender.lock().await.send(Message::Close(None)).await {
                        println!("发送关闭消息失败: {:?}", e);
                    }
                    break;
                } else if input == "wsping" {
                    // 发送WebSocket ping帧
                    println!("发送WebSocket ping帧...");
                    if let Err(e) = ws_sender.lock().await.send(Message::Ping(vec![])).await {
                        println!("发送ping帧错误: {:?}", e);
                        break;
                    } else {
                        println!("ping帧发送成功!");
                    }
                } else if input == "list" {
                    // 列出当前订阅 - 币安支持的格式
                    let list_msg = r#"{"method":"LIST_SUBSCRIPTIONS","id":101}"#;
                    println!("请求当前订阅列表: {}", list_msg);
                    if let Err(e) = ws_sender.lock().await.send(Message::Text(list_msg.to_string())).await {
                        println!("发送列表请求错误: {:?}", e);
                        break;
                    } else {
                        println!("列表请求发送成功!");
                    }
                } else if input.starts_with("sub ") {
                    // 订阅特定数据流
                    let stream = input.trim_start_matches("sub ").trim();
                    if stream.is_empty() {
                        println!("错误: 请指定要订阅的数据流，例如 'sub btcusdt@aggTrade'");
                        continue;
                    }
                    
                    let sub_msg = format!(r#"{{"method":"SUBSCRIBE","params":["{}"],"id":102}}"#, stream);
                    println!("发送订阅请求: {}", sub_msg);
                    if let Err(e) = ws_sender.lock().await.send(Message::Text(sub_msg)).await {
                        println!("发送订阅请求错误: {:?}", e);
                        break;
                    } else {
                        println!("订阅请求发送成功!");
                    }
                } else if input.starts_with("unsub ") {
                    // 取消订阅特定数据流
                    let stream = input.trim_start_matches("unsub ").trim();
                    if stream.is_empty() {
                        println!("错误: 请指定要取消订阅的数据流，例如 'unsub btcusdt@aggTrade'");
                        continue;
                    }
                    
                    let unsub_msg = format!(r#"{{"method":"UNSUBSCRIBE","params":["{}"],"id":103}}"#, stream);
                    println!("发送取消订阅请求: {}", unsub_msg);
                    if let Err(e) = ws_sender.lock().await.send(Message::Text(unsub_msg)).await {
                        println!("发送取消订阅请求错误: {:?}", e);
                        break;
                    } else {
                        println!("取消订阅请求发送成功!");
                    }
                } else if input.starts_with("get ") {
                    // 获取属性值
                    let property = input.trim_start_matches("get ").trim();
                    if property.is_empty() {
                        println!("错误: 请指定要获取的属性，例如 'get combined'");
                        continue;
                    }
                    
                    let get_msg = format!(r#"{{"method":"GET_PROPERTY","params":["{}"],"id":104}}"#, property);
                    println!("发送获取属性请求: {}", get_msg);
                    if let Err(e) = ws_sender.lock().await.send(Message::Text(get_msg)).await {
                        println!("发送获取属性请求错误: {:?}", e);
                        break;
                    } else {
                        println!("获取属性请求发送成功!");
                    }
                } else if input.starts_with("set ") {
                    // 设置属性值
                    let parts: Vec<&str> = input.trim_start_matches("set ").trim().split_whitespace().collect();
                    if parts.len() != 2 {
                        println!("错误: 请指定属性名和值，例如 'set combined true'");
                        continue;
                    }
                    
                    let property = parts[0];
                    let value = parts[1].to_lowercase(); // 转为小写
                    
                    // 检查值是否是合法的布尔值
                    let bool_value = if value == "true" {
                        true
                    } else if value == "false" {
                        false
                    } else {
                        println!("错误: 属性值必须是 true 或 false");
                        continue;
                    };
                    
                    let set_msg = format!(r#"{{"method":"SET_PROPERTY","params":["{}",{}],"id":105}}"#, property, bool_value);
                    println!("发送设置属性请求: {}", set_msg);
                    if let Err(e) = ws_sender.lock().await.send(Message::Text(set_msg)).await {
                        println!("发送设置属性请求错误: {:?}", e);
                        break;
                    } else {
                        println!("设置属性请求发送成功!");
                    }
                } else if input == "ping" {
                    println!("币安WebSocket API不支持PING方法，请使用 'wsping' 发送WebSocket ping帧");
                } else if !input.is_empty() {
                    // 发送原始JSON文本消息
                    println!("发送原始消息: {}", input);
                    if let Err(e) = ws_sender.lock().await.send(Message::Text(input.to_string())).await {
                        println!("发送消息错误: {:?}", e);
                        break;
                    } else {
                        println!("消息发送成功!");
                    }
                }
            }
            
            println!("客户端已关闭");
            Ok(())
        } else {
            return Err("启用了代理但未提供代理地址".into());
        }
    } else {
        println!("不使用代理，直接连接...");
        // 确保清除任何可能存在的代理环境变量
        unsafe {
            std::env::remove_var("HTTP_PROXY");
            std::env::remove_var("HTTPS_PROXY");
        }
        
        return connect_with_env_proxy(url, config, timeout_secs, tls_connector).await;
    }
}

// 通过环境变量设置的代理进行连接(默认方法)
async fn connect_with_env_proxy(url: Url, config: &Config, timeout_secs: u64, tls_connector: Option<native_tls::TlsConnector>) -> Result<(), Box<dyn std::error::Error>> {
    // 构建连接选项，增加超时设置
    let mut request = url.clone().into_client_request()?;
    
    // 设置其他请求头
    let headers = request.headers_mut();
    headers.insert("User-Agent", "RustWebSocketClient/1.0".parse().unwrap());
    
    println!("准备连接请求: {:?}", request);
    
    // 使用tokio-tungstenite的高级API，并指定超时
    println!("开始WebSocket连接...");
    
    println!("设置连接超时: {}秒", timeout_secs);
    let ws_stream_result = tokio::time::timeout(
        Duration::from_secs(timeout_secs),
        connect_async(request)
    ).await;
    
    // 处理连接结果
    let (ws_stream, response) = match ws_stream_result {
        Ok(result) => match result {
            Ok(stream) => stream,
            Err(e) => {
                println!("WebSocket连接错误: {:?}", e);
                println!("请检查网络连接是否正常");
                println!("如果在中国大陆访问，可能需要使用代理");
                return Err(e.into());
            }
        },
        Err(_) => {
            println!("WebSocket连接超时，请检查:");
            println!("1. 网络连接是否正常");
            println!("2. 目标服务器是否可达");
            println!("3. 如果在中国大陆访问，建议使用代理");
            return Err("连接超时".into());
        }
    };
    println!("WebSocket连接成功！");
    println!("服务器响应状态: {}", response.status());
    
    // 分离发送者和接收者
    let (mut ws_sender, mut ws_receiver) = ws_stream.split();
    
    // 包装发送者以便在多个任务间共享
    use std::sync::Arc;
    use tokio::sync::Mutex;
    let ws_sender = Arc::new(Mutex::new(ws_sender));
    let ws_sender_clone = ws_sender.clone();
    
    // 发送几条测试消息
    for i in 1..2 {  // 只发送一条，避免被断开连接
        // 不再发送币安不支持的PING方法，而是直接发送websocket原生的ping帧
        println!("发送WebSocket ping帧...");
        ws_sender.lock().await.send(Message::Ping(vec![])).await?;
        println!("ping帧发送成功!");
        
        // 等待服务器响应
        if let Some(result) = ws_receiver.next().await {
            match result {
                Ok(msg) => {
                    match msg {
                        Message::Text(text) => {
                            println!("收到服务器响应: {}", text);
                        },
                        Message::Binary(data) => println!("收到二进制数据, 长度: {} 字节", data.len()),
                        Message::Pong(_) => println!("收到服务器的Pong响应"),
                        Message::Close(_) => {
                            println!("服务器关闭了连接");
                            return Ok(());
                        },
                        _ => println!("收到其他类型消息: {:?}", msg),
                    }
                },
                Err(e) => {
                    println!("接收消息错误: {:?}", e);
                    return Err(e.into());
                }
            }
        } else {
            println!("WebSocket连接关闭");
            return Ok(());
        }
        
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    
    // 发送订阅请求
    let sub_msg = r#"{"method":"SUBSCRIBE","params":["bnbusdt@aggTrade"],"id":1}"#;
    println!("发送订阅请求: {}", sub_msg);
    ws_sender.lock().await.send(Message::Text(sub_msg.to_string())).await?;
    
    // 等待订阅响应
    if let Some(result) = ws_receiver.next().await {
        match result {
            Ok(msg) => {
                match msg {
                    Message::Text(text) => println!("收到订阅响应: {}", text),
                    _ => println!("收到意外响应类型: {:?}", msg),
                }
            },
            Err(e) => {
                println!("接收订阅响应错误: {:?}", e);
                return Err(e.into());
            }
        }
    }
    
    println!("你可以开始发送消息了，输入 'exit' 退出");
    println!("输入 'sub <symbol>@<stream>' 订阅数据流");
    println!("输入 'unsub <symbol>@<stream>' 取消订阅");
    println!("输入 'wsping' 发送WebSocket ping帧");
    println!("输入 'list' 列出当前订阅");
    println!("输入 'get <property>' 获取属性");
    println!("输入 'set <property> <value>' 设置属性(combined)");
    
    // 创建自动响应ping的任务
    println!("设置自动pong响应（币安要求）...");
    
    // 启动一个异步任务来接收消息并自动响应ping
    let _receive_task = tokio::spawn(async move {
        // 记录上次发送pong的时间
        let mut last_pong_time = std::time::Instant::now();
        
        while let Some(result) = ws_receiver.next().await {
            match result {
                Ok(msg) => {
                    match msg {
                        Message::Text(text) => println!("收到消息: {}", text),
                        Message::Binary(data) => println!("收到二进制数据, 长度: {} 字节", data.len()),
                        Message::Ping(data) => {
                            println!("收到Ping，自动响应Pong");
                            // 自动响应pong，币安要求在10分钟内回复
                            if let Err(e) = ws_sender_clone.lock().await.send(Message::Pong(data)).await {
                                println!("发送Pong失败: {:?}", e);
                                break;
                            }
                            last_pong_time = std::time::Instant::now();
                        },
                        Message::Pong(_) => {
                            println!("收到Pong响应");
                            last_pong_time = std::time::Instant::now();
                        },
                        Message::Close(_) => {
                            println!("服务器关闭了连接");
                            break;
                        },
                        _ => println!("收到其他类型消息: {:?}", msg),
                    }
                    
                    // 每3分钟主动发送一次pong，保持连接
                    // 币安服务器每3分钟发送ping，我们主动每3分钟发送pong以确保连接不会断开
                    if last_pong_time.elapsed() > Duration::from_secs(180) {
                        println!("发送保活Pong（币安连接要求）");
                        if let Err(e) = ws_sender_clone.lock().await.send(Message::Pong(vec![])).await {
                            println!("发送保活Pong失败: {:?}", e);
                            break;
                        }
                        last_pong_time = std::time::Instant::now();
                    }
                },
                Err(e) => {
                    println!("接收消息错误: {:?}", e);
                    break;
                }
            }
        }
        
        println!("接收任务结束");
    });
    
    // 主循环处理用户输入
    loop {
        // 提示用户输入
        print!("> ");
        io::stdout().flush().unwrap();
        
        let mut input = String::new();
        io::stdin().read_line(&mut input).unwrap();
        
        // 去除尾部换行符
        let input = input.trim();
        if input == "exit" {
            // 发送关闭消息
            println!("退出命令，准备关闭连接");
            if let Err(e) = ws_sender.lock().await.send(Message::Close(None)).await {
                println!("发送关闭消息失败: {:?}", e);
            }
            break;
        } else if input == "wsping" {
            // 发送WebSocket ping帧
            println!("发送WebSocket ping帧...");
            if let Err(e) = ws_sender.lock().await.send(Message::Ping(vec![])).await {
                println!("发送ping帧错误: {:?}", e);
                break;
            } else {
                println!("ping帧发送成功!");
            }
        } else if input == "list" {
            // 列出当前订阅 - 币安支持的格式
            let list_msg = r#"{"method":"LIST_SUBSCRIPTIONS","id":101}"#;
            println!("请求当前订阅列表: {}", list_msg);
            if let Err(e) = ws_sender.lock().await.send(Message::Text(list_msg.to_string())).await {
                println!("发送列表请求错误: {:?}", e);
                break;
            } else {
                println!("列表请求发送成功!");
            }
        } else if input.starts_with("sub ") {
            // 订阅特定数据流
            let stream = input.trim_start_matches("sub ").trim();
            if stream.is_empty() {
                println!("错误: 请指定要订阅的数据流，例如 'sub btcusdt@aggTrade'");
                continue;
            }
            
            let sub_msg = format!(r#"{{"method":"SUBSCRIBE","params":["{}"],"id":102}}"#, stream);
            println!("发送订阅请求: {}", sub_msg);
            if let Err(e) = ws_sender.lock().await.send(Message::Text(sub_msg)).await {
                println!("发送订阅请求错误: {:?}", e);
                break;
            } else {
                println!("订阅请求发送成功!");
            }
        } else if input.starts_with("unsub ") {
            // 取消订阅特定数据流
            let stream = input.trim_start_matches("unsub ").trim();
            if stream.is_empty() {
                println!("错误: 请指定要取消订阅的数据流，例如 'unsub btcusdt@aggTrade'");
                continue;
            }
            
            let unsub_msg = format!(r#"{{"method":"UNSUBSCRIBE","params":["{}"],"id":103}}"#, stream);
            println!("发送取消订阅请求: {}", unsub_msg);
            if let Err(e) = ws_sender.lock().await.send(Message::Text(unsub_msg)).await {
                println!("发送取消订阅请求错误: {:?}", e);
                break;
            } else {
                println!("取消订阅请求发送成功!");
            }
        } else if input.starts_with("get ") {
            // 获取属性值
            let property = input.trim_start_matches("get ").trim();
            if property.is_empty() {
                println!("错误: 请指定要获取的属性，例如 'get combined'");
                continue;
            }
            
            let get_msg = format!(r#"{{"method":"GET_PROPERTY","params":["{}"],"id":104}}"#, property);
            println!("发送获取属性请求: {}", get_msg);
            if let Err(e) = ws_sender.lock().await.send(Message::Text(get_msg)).await {
                println!("发送获取属性请求错误: {:?}", e);
                break;
            } else {
                println!("获取属性请求发送成功!");
            }
        } else if input.starts_with("set ") {
            // 设置属性值
            let parts: Vec<&str> = input.trim_start_matches("set ").trim().split_whitespace().collect();
            if parts.len() != 2 {
                println!("错误: 请指定属性名和值，例如 'set combined true'");
                continue;
            }
            
            let property = parts[0];
            let value = parts[1].to_lowercase(); // 转为小写
            
            // 检查值是否是合法的布尔值
            let bool_value = if value == "true" {
                true
            } else if value == "false" {
                false
            } else {
                println!("错误: 属性值必须是 true 或 false");
                continue;
            };
            
            let set_msg = format!(r#"{{"method":"SET_PROPERTY","params":["{}",{}],"id":105}}"#, property, bool_value);
            println!("发送设置属性请求: {}", set_msg);
            if let Err(e) = ws_sender.lock().await.send(Message::Text(set_msg)).await {
                println!("发送设置属性请求错误: {:?}", e);
                break;
            } else {
                println!("设置属性请求发送成功!");
            }
        } else if input == "ping" {
            println!("币安WebSocket API不支持PING方法，请使用 'wsping' 发送WebSocket ping帧");
        } else if !input.is_empty() {
            // 发送原始JSON文本消息
            println!("发送原始消息: {}", input);
            if let Err(e) = ws_sender.lock().await.send(Message::Text(input.to_string())).await {
                println!("发送消息错误: {:?}", e);
                break;
            } else {
                println!("消息发送成功!");
            }
        }
    }
    
    println!("客户端已关闭");
    Ok(())
}

fn main() {
    // 使用clap进行命令行参数解析
    let matches = Command::new("WebSocket客户端")
        .version("1.0")
        .about("WebSocket客户端，支持直连和代理连接")
        .arg(Arg::new("url")
            .short('u')
            .long("url")
            .help("WebSocket服务器地址")
            .default_value("wss://fstream.binance.com/stream"))
        .arg(Arg::new("preset")
            .short('s')
            .long("preset")
            .help("预设WebSocket服务器 (binance, binance_spot, echo, piesocket, local)")
            .default_value("binance"))
        .arg(Arg::new("proxy")
            .short('p')
            .long("proxy")
            .help("代理服务器地址 (http://host:port 或 socks5://host:port)")
            .default_value("http://127.0.0.1:7890"))
        .arg(Arg::new("no-proxy")
            .long("no-proxy")
            .help("不使用代理，直接连接")
            .action(clap::ArgAction::SetTrue))
        .arg(Arg::new("timeout")
            .short('t')
            .long("timeout")
            .help("连接超时时间(秒)")
            .default_value("15"))
        .get_matches();
    
    // 处理预设服务器选项
    let url = if matches.get_one::<String>("url").unwrap() == "wss://fstream.binance.com/ws/bnbusdt@aggTrade" {
        // 如果URL是默认值，检查是否指定了预设服务器
        let preset_name = matches.get_one::<String>("preset").unwrap().as_str();
        if let Some(server_info) = WS_SERVERS.get(preset_name) {
            println!("使用预设服务器: {} - {}", preset_name, server_info.description);
            
            // 对于币安服务器，自动添加交易对信息
            if preset_name.starts_with("binance") {
                format!("{}/ws/bnbusdt@aggTrade", server_info.base_url)
            } else {
                server_info.base_url.to_string()
            }
        } else {
            println!("未找到预设服务器: {}, 使用默认URL", preset_name);
            matches.get_one::<String>("url").unwrap().clone()
        }
    } else {
        // 如果指定了URL，使用指定的URL
        matches.get_one::<String>("url").unwrap().clone()
    };
    
    // 列出所有可用的预设服务器
    println!("可用的预设服务器:");
    for (name, info) in WS_SERVERS.iter() {
        println!("  {} - {}", name, info.description);
    }
    
    // 检测代理类型
    let proxy_type = if matches.get_flag("no-proxy") {
        ProxyType::None
    } else {
        let proxy_url_str = matches.get_one::<String>("proxy").unwrap().clone();
        if proxy_url_str.starts_with("socks5://") {
            ProxyType::Socks5(proxy_url_str.clone())
        } else {
            ProxyType::Http(proxy_url_str.clone())
        }
    };
    
    // 配置初始化
    let mut config = Config {
        server_url: url,
        use_proxy: !matches.get_flag("no-proxy"), 
        proxy_url: if matches.get_flag("no-proxy") { 
            None 
        } else { 
            Some(matches.get_one::<String>("proxy").unwrap().clone()) 
        },
        proxy_type: proxy_type,
    };

    // 打印配置信息
    println!("WebSocket客户端启动中...");
    println!("服务器地址: {}", config.server_url);
    println!("代理状态: {}", if config.use_proxy { "启用" } else { "禁用" });
    match &config.proxy_type {
        ProxyType::None => println!("不使用代理"),
        ProxyType::Http(url) => println!("使用HTTP代理: {}", url),
        ProxyType::Socks5(url) => println!("使用SOCKS5代理: {}", url),
    }
    
    // 创建一个tokio运行时来支持异步操作
    let rt = Runtime::new().unwrap();
    
    // 获取超时设置
    let timeout_secs = matches.get_one::<String>("timeout")
        .unwrap()
        .parse::<u64>()
        .unwrap_or(15);
    
    // 在tokio运行时中执行所有异步操作
    if let Err(e) = rt.block_on(connect_to_server(&config, timeout_secs)) {
        eprintln!("错误: {}", e);
    }
} 