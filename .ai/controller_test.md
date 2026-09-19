• 对，先测 open。它能验证启动握手和 Tuzi → controller 的定向消息；然后再测这次
  修改的状态事件路由。测试二进制已编译好。

  开三个终端，均进入 /users/2533494w/projects/tuzi：

  终端 A：启动 controller

  ./target/debug/tu dds controller

  记下 controller-ready 中的 peer_id，假设是 701。在同一终端输入一整行：

  {"request_id":1,"op":"register","token":"manual-test-1"}

  终端 B：启动受控 Tuzi

  把 701 换成实际 ID：

  TUZI_DDS_PARENT=701 TUZI_DDS_TOKEN=manual-test-1 \
    ./target/debug/tuzi --runtime-config '{"config":{"dds":
    {"open":"parent"}}}' .

  终端 A 应收到 tuzi-ready。在 Tuzi 中移到一个文件，按 o；终端 A 应收到
  kind:"open"，其中有文件路径。

  终端 C：验证状态事件没有公开广播

  ./target/debug/tu dds sub --json

  回到 Tuzi，用 j/k 移动光标。终端 A 应收到 kind:"hover"；终端 C 不应收到
  Hover 消息。终端 C 出现 Sync 属于正常现象。

  最后可验证公开广播：退出 Tuzi，在终端 A 登记新 token，用 broadcast:["hover"]
  重新启动。此时移动光标，终端 C 应收到 Hover，其 receiver 为 0；终端 A 只应收
  到一份对应消息。手动测试完成后，实际插件启动时应改用随机、一次性的 token。
