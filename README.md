# NAS Video Editor

轻量级 NAS 视频剪辑工具，Rust (Axum) 后端 + 原生 JS 前端，内存占用 ~2MB。

## 功能

- 📁 **文件浏览** — 浏览 NAS 上的视频文件，支持多目录挂载
- 🎬 **视频预览** — 拖动进度条、逐帧定位、.ts 文件原生播放（mpegts.js）
- ✂️ **片段剪辑** — 标记入点/出点，支持 copy / GPU / CPU 编码
- 🔗 **多段拼接** — 选取多段拼接输出
- 🚀 **GPU 加速** — NVIDIA NVENC / Intel QSV / AMD VAAPI 自动检测
- ⚡ **Stream Copy** — 不重编码，秒级完成
- 📐 **转码** — 改分辨率、输出格式、自定义 FFmpeg 参数
- 🔒 **密码认证** — Cookie + HMAC token，7 天有效期
- 🌗 **暗色/亮色主题** — shadcn-slate 风格

## 快速开始

### Docker Compose（推荐）

```yaml
services:
  video-editor:
    image: aimercat1994/nas-video-editor:latest
    container_name: video-editor
    restart: unless-stopped
    ports:
      - "8090:8080"
    volumes:
      - /path/to/your/videos:/videos
      - video-editor-data:/data
    environment:
      - PASSWORD=your-password
      - TZ=Asia/Shanghai

volumes:
  video-editor-data:
```

```bash
docker compose up -d
# 访问 http://NAS_IP:8090
```

### Docker Run

```bash
docker run -d \
  --name video-editor \
  --restart unless-stopped \
  -p 8090:8080 \
  -e PASSWORD=your-password \
  -e TZ=Asia/Shanghai \
  -v /path/to/your/videos:/videos \
  aimercat1994/nas-video-editor:latest
```

## GPU 支持

### NVIDIA（独显）

需要宿主机安装 [NVIDIA Container Toolkit](https://docs.nvidia.com/datacenter/cloud-native/container-toolkit/install-guide.html)。

```yaml
services:
  video-editor:
    image: aimercat1994/nas-video-editor:latest
    ports:
      - "8090:8080"
    volumes:
      - /path/to/videos:/videos
      - video-editor-data:/data
    environment:
      - PASSWORD=your-password
    deploy:
      resources:
        reservations:
          devices:
            - driver: nvidia
              count: all
              capabilities: [gpu]
```

### Intel QSV（核显）

```yaml
services:
  video-editor:
    image: aimercat1994/nas-video-editor:latest
    ports:
      - "8090:8080"
    volumes:
      - /path/to/videos:/videos
      - video-editor-data:/data
    environment:
      - PASSWORD=your-password
    devices:
      - /dev/dri:/dev/dri
```

### AMD VAAPI

同 Intel QSV 配置，添加 `devices: /dev/dri:/dev/dri`。

## 编码器对比

| 编码器 | 类型 | 速度 | 适用场景 |
|--------|------|------|----------|
| `copy` | 不重编码 | ⚡ 秒级 | 快速裁剪（默认） |
| `libx264` | CPU | 🐢 慢 | 精准剪辑、无 GPU |
| `libx265` | CPU | 🐢 很慢 | 高压缩比 |
| `h264_nvenc` | NVIDIA | 🚀 快 | 快速精准剪辑 |
| `hevc_nvenc` | NVIDIA | 🚀 快 | 高压缩 + 快速 |
| `h264_qsv` | Intel | 🚀 快 | NAS 常用 |
| `hevc_qsv` | Intel | 🚀 快 | NAS 高压缩 |
| `h264_vaapi` | AMD | 🚀 较快 | AMD 设备 |
| `hevc_vaapi` | AMD | 🚀 较快 | AMD 高压缩 |

## 环境变量

| 变量 | 默认值 | 说明 |
|------|--------|------|
| `PORT` | `8080` | 容器内监听端口 |
| `VIDEOS_DIR` | `/videos` | 视频目录挂载点 |
| `PASSWORD` | （空） | 登录密码，留空则无需认证 |
| `TZ` | `UTC` | 时区 |

## 快捷键

| 按键 | 功能 |
|------|------|
| `Space` | 播放 / 暂停 |
| `←` `→` | 逐帧前进/后退 |
| `I` | 标记入点 |
| `O` | 标记出点 |
| `A` | 添加片段 |

## 输出文件

输出文件保存在源文件同目录：
- 单段剪辑：`原文件名_CLIP_时间戳.mp4`
- 多段拼接：`原文件名_CONCAT_时间戳.mp4`

## 技术栈

- **后端**: Rust + Axum + Tokio + FFmpeg
- **前端**: Vanilla JS + CSS Custom Properties
- **容器**: Debian trixie-slim + FFmpeg 7.x
- **内存占用**: ~2MB

## License

MIT
