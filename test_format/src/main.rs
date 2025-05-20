fn main() {
    let host = "example.com";
    let port = 443;
    let ws_key = "dummy_key";
    let path_query = "/ws";
    
    let request = format!(
        "GET {} HTTP/1.1\r\n\
        Host: {}:{}\r\n\
        Connection: Upgrade\r\n\
        Upgrade: websocket\r\n\
        Sec-WebSocket-Version: 13\r\n\
        Sec-WebSocket-Key: {}\r\n\
        Origin: http://{}:{}\r\n\r\n",
        path_query, host, port, ws_key, host, port
    );
    
    println!("{}", request);
}
