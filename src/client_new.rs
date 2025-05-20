use std::io::{self, Write};
use std::time::Duration;
use std::collections::HashMap;
use std::sync::Arc;
use clap::{Arg, Command};
use url::Url;
use tungstenite::Message;
use serde_json::{json, Value};

// tokio相关依赖
use tokio::net::TcpStream;
use tokio::runtime::Runtime;
use tokio::sync::Mutex;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio_tungstenite::{connect_async};
use futures_util::{SinkExt, StreamExt};

// 配置结构体
struct Config {
    server_url: String,       // WebSocket服务器地址
    use_proxy: bool,          // 是否使用代理
    proxy_url: Option<String>, // 代理服务器地址
    auto_shutdown: Option<u64>, // 自动关闭时间（秒）
    symbol: String,           // 交易对
    streams: Vec<String>,     // 要订阅的数据流
}

// WebSocket服务器信息
struct BinanceWSInfo {
    base_url: &'static str,
    description: &'static str,
}

// 预设的WebSocket服务器列表
lazy_static::lazy_static! {
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
    let proxy_host = proxy_url.host_str().unwrap_or("127.0.0.1");
    let proxy_port = proxy_url.port().unwrap_or(7890);
    
    println!("代理服务器: {}:{}", proxy_host, proxy_port);
    
    // 连接到代理服务器
    println!("正在连接到代理服务器...");
    let proxy_addr = format!("{}:{}", proxy_host, proxy_port);
    
    // 根据代理类型选择不同的连接方式
    if proxy_url.scheme() == "http" || proxy_url.scheme() == "https" {
        println!("使用HTTP(S)代理，发送CONNECT请求");
        let mut tcp_stream = TcpStream::connect(proxy_addr).await?;
        
        // 目标地址
        let target_addr = format!("{}:{}", target_host, target_port);
        
        // 构建CONNECT请求
        let connect_req = format!(
            "CONNECT {} HTTP/1.1\r\n\
            Host: {}\r\n\
            User-Agent: rustws-client\r\n\
            Proxy-Connection: Keep-Alive\r\n\r\n",
            target_addr, target_addr
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
    } else if proxy_url.scheme() == "socks5" {
        println!("使用SOCKS5代理");
        // 使用tokio-socks库连接SOCKS5代理
        let proxy_addr = (proxy_host.to_string(), proxy_port);
        let target_addr = (target_host.to_string(), target_port);
        
        println!("连接SOCKS5代理: {:?}", proxy_addr);
        println!("目标服务器: {:?}", target_addr);
        
        // 连接到SOCKS5代理
        let stream = tokio_socks::tcp::Socks5Stream::connect(
            proxy_addr,
            target_addr
        ).await.map_err(|e| {
            println!("SOCKS5代理连接失败: {:?}", e);
            e
        })?;
        
        println!("SOCKS5代理连接成功");
        Ok(stream.into_inner())
    } else {
        return Err(format!("不支持的代理协议: {}", proxy_url.scheme()).into());
    }
}

// 币安API操作类型
enum BinanceApiOperation {
    Subscribe(Vec<String>),     // 订阅
    Unsubscribe(Vec<String>),   // 取消订阅
    ListSubscriptions,          // 列出当前订阅
    GetProperty(String),        // 获取属性
    SetProperty(String, Value), // 设置属性
}

// 生成币安API请求JSON
fn create_binance_api_request(operation: BinanceApiOperation) -> String {
    let (method, params) = match operation {
        BinanceApiOperation::Subscribe(streams) => {
            ("SUBSCRIBE", json!(streams))
        },
        BinanceApiOperation::Unsubscribe(streams) => {
            ("UNSUBSCRIBE", json!(streams))
        },
        BinanceApiOperation::ListSubscriptions => {
            ("LIST_SUBSCRIPTIONS", json!(null))
        },
        BinanceApiOperation::GetProperty(property) => {
            ("GET_PROPERTY", json!(property))
        },
        BinanceApiOperation::SetProperty(property, value) => {
            ("SET_PROPERTY", json!({property: value}))
        },
    };
    
    // 生成请求ID (简单使用时间戳)
    let id = chrono::Utc::now().timestamp_millis();
    
    json!({
        "method": method,
        "params": params,
        "id": id
    }).to_string()
}

// 连接到WebSocket服务器
async fn connect_to_server(config: &Config, timeout_secs: u64) -> Result<(), Box<dyn std::error::Error>> {
    let url = Url::parse(&config.server_url)?;
    println!("连接到WebSocket服务器: {}", url);
    println!("使用tokio-tungstenite的高级API连接WebSocket...");
    
    // 使用代理设置环境变量
    if config.use_proxy {
        if let Some(proxy_url_str) = &config.proxy_url {
            println!("使用代理: {}", proxy_url_str);
            
            // 设置代理环境变量
            std::env::set_var("HTTP_PROXY", proxy_url_str);
            std::env::set_var("HTTPS_PROXY", proxy_url_str);
            std::env::set_var("WSS_PROXY", proxy_url_str); // 特别为WebSocket添加
            // 添加NO_PROXY环境变量，避免对本地地址使用代理
            std::env::set_var("NO_PROXY", "localhost,127.0.0.1,::1");
            
            println!("已设置代理环境变量，尝试连接...");
        } else {
            return Err("启用了代理但未提供代理地址".into());
        }
    } else {
        println!("不使用代理，直接连接...");
        // 确保清除任何可能存在的代理环境变量
        std::env::remove_var("HTTP_PROXY");
        std::env::remove_var("HTTPS_PROXY");
    }
    
    // 构建连接选项，增加超时设置
    use tokio_tungstenite::tungstenite::client::IntoClientRequest;
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
                if config.use_proxy {
                    println!("请检查代理服务器 {} 是否正常工作", config.proxy_url.as_ref().unwrap());
                    println!("尝试使用 --no-proxy 参数直接连接");
                } else {
                    println!("请检查网络连接是否正常");
                    println!("如果在中国大陆访问，可能需要使用代理");
                }
                return Err(e.into());
            }
        },
        Err(_) => {
            println!("WebSocket连接超时，请检查:");
            println!("1. 网络连接是否正常");
            println!("2. 代理服务器是否可用");
            println!("3. 目标服务器是否可达");
            return Err("连接超时".into());
        }
    };
    println!("WebSocket连接成功！");
    println!("服务器响应状态: {}", response.status());
    
    // 分离发送者和接收者
    let (ws_sender, mut ws_receiver) = ws_stream.split();
    
    // 包装发送者以便在多个任务间共享
    let ws_sender = Arc::new(Mutex::new(ws_sender));
    let ws_sender_clone = ws_sender.clone();
    
    // 检测是否是币安API
    let is_binance_api = config.server_url.contains("binance.com");
    if is_binance_api {
        println!("检测到币安API连接，自动订阅数据流...");
        
        // 打印订阅的数据流
        println!("交易对: {}", config.symbol);
        println!("要订阅的数据流: {:?}", config.streams);
        
        // 构建完整的数据流名称
        let full_streams: Vec<String> = config.streams.iter()
            .map(|stream| format!("{}{}", config.symbol.to_lowercase(), stream))
            .collect();
        
        println!("完整的数据流名称: {:?}", full_streams);
        
        // 发送订阅请求
        let subscribe_req = create_binance_api_request(
            BinanceApiOperation::Subscribe(full_streams)
        );
        println!("发送订阅请求: {}", subscribe_req);
        ws_sender.lock().await.send(Message::Text(subscribe_req)).await?;
        
        // 添加列出当前订阅的请求
        println!("获取当前订阅列表...");
        let list_req = create_binance_api_request(BinanceApiOperation::ListSubscriptions);
        ws_sender.lock().await.send(Message::Text(list_req)).await?;
    } else {
        // 发送一条测试消息
        let test_msg = "测试消息";
        println!("发送测试消息: {}", test_msg);
        ws_sender.lock().await.send(Message::Text(test_msg.to_string())).await?;
        println!("测试消息发送成功!");
    }
    
    println!("开始接收服务器数据...");
    println!("设置自动ping/pong响应（保持长连接）...");
    
    // 记录是否已连接的标志
    let connected = Arc::new(Mutex::new(true));
    let connected_clone = connected.clone();
    
    // 创建关闭信号
    let shutdown = Arc::new(Mutex::new(false));
    let shutdown_clone = shutdown.clone();
    
    // 如果设置了自动关闭时间，则启动定时器
    if let Some(auto_shutdown_secs) = config.auto_shutdown {
        println!("已设置自动关闭，将在{}秒后关闭连接", auto_shutdown_secs);
        
        let shutdown_inner = shutdown.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_secs(auto_shutdown_secs)).await;
            println!("达到预设时间，准备自动关闭连接...");
            *shutdown_inner.lock().await = true;
        });
    }
    
    // 启动断线重连检测任务
    let ping_sender = ws_sender.clone();
    let reconnect_task = tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(60));
        loop {
            interval.tick().await;
            
            // 检查是否需要关闭
            if *shutdown_clone.lock().await {
                println!("收到关闭信号，停止ping发送");
                break;
            }
            
            // 检查连接状态
            if !*connected_clone.lock().await {
                println!("检测到连接已断开，停止重连检测");
                break;
            }
            
            // 发送ping保持连接
            println!("发送保活Ping...");
            if let Err(e) = ping_sender.lock().await.send(Message::Ping(vec![])).await {
                println!("发送Ping失败: {:?}", e);
                *connected_clone.lock().await = false;
                break;
            }
        }
    });
    
    // 启动一个异步任务来接收消息并自动响应ping
    let shutdown_receiver = shutdown.clone();
    let receive_task = tokio::spawn(async move {
        // 记录上次发送pong的时间
        let mut last_pong_time = std::time::Instant::now();
        // 接收到的消息数
        let mut message_count = 0;
        
        while let Some(result) = ws_receiver.next().await {
            // 检查是否需要关闭
            if *shutdown_receiver.lock().await {
                println!("收到关闭信号，停止接收消息");
                break;
            }
            
            match result {
                Ok(msg) => {
                    match msg {
                        Message::Text(text) => {
                            // 尝试解析为JSON，判断是否是币安API响应
                            if let Ok(json) = serde_json::from_str::<Value>(&text) {
                                if json.get("result").is_some() || json.get("id").is_some() {
                                    println!("收到API响应: {}", text);
                                } else {
                                    message_count += 1;
                                    if message_count % 10 == 0 {
                                        println!("已接收 {} 条数据，最新: {}", message_count, text);
                                    }
                                }
                            } else {
                                println!("收到消息: {}", text);
                            }
                        },
                        Message::Binary(data) => println!("收到二进制数据, 长度: {} 字节", data.len()),
                        Message::Ping(data) => {
                            println!("收到Ping，自动响应Pong");
                            // 自动响应pong
                            if let Err(e) = ws_sender_clone.lock().await.send(Message::Pong(data)).await {
                                println!("发送Pong失败: {:?}", e);
                                *connected.lock().await = false;
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
                            *connected.lock().await = false;
                            break;
                        },
                        _ => println!("收到其他类型消息: {:?}", msg),
                    }
                    
                    // 每5分钟主动发送一次pong，保持连接
                    if last_pong_time.elapsed() > Duration::from_secs(300) {
                        println!("发送保活Pong");
                        if let Err(e) = ws_sender_clone.lock().await.send(Message::Pong(vec![])).await {
                            println!("发送保活Pong失败: {:?}", e);
                            *connected.lock().await = false;
                            break;
                        }
                        last_pong_time = std::time::Instant::now();
                    }
                },
                Err(e) => {
                    println!("接收消息错误: {:?}", e);
                    *connected.lock().await = false;
                    break;
                }
            }
        }
        
        println!("接收任务结束，共接收 {} 条数据", message_count);
    });
    
    // 等待直到接收到关闭信号
    while !*shutdown.lock().await {
        // 检查连接状态
        if !*connected.lock().await {
            println!("连接已断开，退出等待");
            break;
        }
        
        // 每隔1秒检查一次
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    
    // 发送关闭消息
    println!("准备关闭连接...");
    let _ = ws_sender.lock().await.send(Message::Close(None)).await;
    *connected.lock().await = false;
    
    // 等待接收任务完成
    println!("正在等待接收任务完成...");
    let _ = receive_task.await;
    let _ = reconnect_task.await;
    
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
            .default_value("wss://fstream.binance.com/ws"))
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
        .arg(Arg::new("duration")
            .short('d')
            .long("duration")
            .help("自动运行时长(秒)，到达时间后自动关闭")
            .default_value("300"))
        .arg(Arg::new("symbol")
            .short('S')
            .long("symbol")
            .help("交易对，例如 btcusdt")
            .default_value("bnbusdt"))
        .arg(Arg::new("streams")
            .long("streams")
            .help("要订阅的数据流，例如 @aggTrade,@depth")
            .default_value("@aggTrade"))
        .get_matches();
    
    // 处理预设服务器选项
    let url = if matches.get_one::<String>("url").unwrap() == "wss://fstream.binance.com/ws" {
        // 如果URL是默认值，检查是否指定了预设服务器
        let preset_name = matches.get_one::<String>("preset").unwrap().as_str();
        if let Some(server_info) = WS_SERVERS.get(preset_name) {
            println!("使用预设服务器: {} - {}", preset_name, server_info.description);
            
            // 对于币安服务器，使用通用的/ws端点
            if preset_name.starts_with("binance") {
                format!("{}/ws", server_info.base_url)
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
    
    // 获取自动关闭时间
    let auto_shutdown = matches.get_one::<String>("duration")
        .unwrap()
        .parse::<u64>()
        .unwrap_or(300);
    
    // 获取交易对和数据流
    let symbol = matches.get_one::<String>("symbol")
        .unwrap()
        .clone();
    
    let streams: Vec<String> = matches.get_one::<String>("streams")
        .unwrap()
        .split(',')
        .map(|s| s.trim().to_string())
        .collect();
    
    // 配置初始化 - 默认禁用代理
    let mut config = Config {
        server_url: url,
        use_proxy: false, // 直接在这里设置为false，默认不使用代理
        proxy_url: None,
        auto_shutdown: Some(auto_shutdown),
        symbol,
        streams,
    };
    
    // 如果命令行中显式指定了使用代理（没有--no-proxy参数），则启用代理
    if !matches.get_flag("no-proxy") {
        config.use_proxy = true;
        config.proxy_url = Some(matches.get_one::<String>("proxy").unwrap().clone());
    }

    // 打印配置信息
    println!("WebSocket客户端启动中...");
    println!("服务器地址: {}", config.server_url);
    println!("代理状态: {}", if config.use_proxy { "启用" } else { "禁用" });
    if let Some(proxy) = &config.proxy_url {
        println!("代理地址: {}", proxy);
    }
    println!("自动运行时长: {}秒", auto_shutdown);
    println!("交易对: {}", config.symbol);
    println!("数据流: {:?}", config.streams);
    
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
