
# `fusen-net` 轻量级内网穿透工具

fusen-net是一个轻量级内网穿透工具，可以通过简单的方式实现高性能内网穿透服务。

## 功能列表

- :white_check_mark: 内网多端口代理
- :white_check_mark: TCP内网穿透
- :construction: 连接分组/鉴权
- :construction: UDP-P2P内网穿透

## 快速开始

### 网络拓扑图

![网络拓扑图](https://github.com/kwsc98/fusen-net/blob/main/images/net-work.jpeg?raw=true)

## Server

```rust
cd ../fusen-net/target/release/
./server -p 8089
----------------------------------------------------------------------------------
-p / --port : Server服务监听端口
```

fusen-net-server通过指定--port参数进行启动，默认为8089

## client-agent1

```rust
cd ../fusen-net/target/release/
./client -s 120.46.75.13:8089 -t agent1
----------------------------------------------------------------------------------
-s / --server_host : Server服务地址
-t / --tag : agent标识
```

## client-agent2

```rust
cd ../fusen-net/target/release/
./client -s 120.46.75.13:8089 -t agent2 -a agent1-0.0.0.0:8081-8078
----------------------------------------------------------------------------------
-s / --server_host : Server服务地址
-t / --tag : agent标识
-a / --agent : 代理目标与绑定端口配置格式为 {目标Tag标识}-{目前内网Host}-{代理端口} ,支持多端口代理可以指定多个 --agent
```

Server与agent启动后，TcpClient就可以调用本地的127.0.0.1:8078端口，来对TcpServer暴露的0.0.0.0:8081端口进行内网穿透调用。

# Docker
本项目也支持Docker镜像部署方式

```rust
//server
docker run --name fusen-net-server -p 8089:8089 kwsc98/fusen-net-server:latest

//Client-agent1
docker run --name fusen-net-agent1 -e SERVER_HOST=120.46.75.13:8089 -e TAG=agent1 kwsc98/fusen-net-client:latest

//Client-agent2
docker run --name fusen-net-agent2 -e SERVER_HOST=120.46.75.13:8089 -e TAG=agent2 -e AGENTS=agent1-0.0.0.0:8081-8078,agent1-0.0.0.0:8082-8079 -p 8078:8078 -p 8079:8079 kwsc98/fusen-net-client:latest
```