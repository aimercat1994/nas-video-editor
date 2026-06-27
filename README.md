# NAS Video Editor

轻量级 NAS 视频剪辑工具 — Rust (Axum) 后端 + 原生 JS 前端，内存占用 **~2MB**。

基于 FFmpeg，支持 GPU 加速裁剪、多段拼接、转码、流式播放，开箱即用。

## 功能

**播放器**
- 🎬 HTML5 视频播放器，支持进度条拖拽、逐帧定位（可调步进 1帧/0.1s/1s/5s/10s）
- 📺 .ts (MPEG-TS) 文件原生播放（mpegts.js，无需服务端转封装）
- 📊 视频信息栏：分辨率、编码、帧率、码率、时长、大小、音频信息

**剪辑**
- ✂️ 标记入点/出点，添加到片段队列，支持单段剪辑和多段拼接
- ⚡ Stream Copy 模式 — 不重编码，秒级完成
- 🚀 GPU 加速 — NVIDIA NVENC / Intel QSV / AMD VAAPI，启动时自动检测
- 📐 转码输出 — 可选分辨率（4K/1080p/720p/480p）、格式（MP4/MKV/WebM/AVI/MOV）
- 🔧 自定义 FFmpeg 额外参数

**系统**
- 🔒 密码认证 — HMAC-SHA256 token，Cookie 7 天有效期
- 🌗 暗色/亮色主题 — shadcn-slate 风格
- 📋 任务队列 — 异步任务管理，实时状态更新
- 📁 文件浏览器 — 多目录挂载，面包屑导航
- 🪶 极低资源占用 — Rust 编译二进制 ~12MB，运行内存 ~2MB

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

### 多目录挂载

```yaml
volumes:
  - /path/to/movies:/videos/movies
  - /path/to/tv-shows:/videos/tv-shows
  - /path/to/backups:/videos/backups
```

## GPU 支持

### NVIDIA（独显）

适用于 RTX 3060/4060、GTX 1650 等。需要宿主机安装 [NVIDIA Container Toolkit](https://docs.nvidia.com/datacenter/cloud-native/container-toolkit/install-guide.html)。

```bash
# 检查驱动
nvidia-smi

# 安装 toolkit（如未安装）
curl -fsSL https://nvidia.github.io/libnvidia-container/gpgkey | \
  sudo gpg --dearmor -o /usr/share/keyrings/nvidia-container-toolkit-keyring.gpg
curl -s -L https://nvidia.github.io/libnvidia-container/stable/deb/nvidia-container-toolkit.list | \
  sed 's#deb https://#deb [signed-by=/usr/share/keyrings/nvidia-container-toolkit-keyring.gpg] https://#g' | \
  sudo tee /etc/apt/sources.list.d/nvidia-container-toolkit.list
sudo apt-get update && sudo apt-get install -y nvidia-container-toolkit
sudo nvidia-ctk runtime configure --runtime=docker
sudo systemctl restart docker
```

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

适用于 N100/N305/i3/i5/i7 等带核显的 CPU，NAS 最常见方案。

```bash
# 检查设备
ls /dev/dri/renderD128
```

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

适用于 AMD GPU 或 APU，配置同 Intel QSV。

## 编码器对比

| 编码器 | 类型 | 速度 | 质量 | 适用场景 |
|--------|------|------|------|----------|
| `copy` | 不重编码 | ⚡ 秒级 | 原始 | 快速裁剪（默认） |
| `libx264` | CPU | 🐢 慢 | 高 | 精准剪辑、无 GPU |
| `libx265` | CPU | 🐢 很慢 | 最高 | 高压缩比 |
| `h264_nvenc` | NVIDIA | 🚀 快 | 良好 | 快速精准剪辑 |
| `hevc_nvenc` | NVIDIA | 🚀 快 | 良好 | 高压缩 + 快速 |
| `h264_qsv` | Intel | 🚀 快 | 良好 | NAS 常用 |
| `hevc_qsv` | Intel | 🚀 快 | 良好 | NAS 高压缩 |
| `h264_vaapi` | AMD | 🚀 较快 | 良好 | AMD 设备 |
| `hevc_vaapi` | AMD | 🚀 较快 | 良好 | AMD 高压缩 |

> **Stream Copy vs 转码**：`copy` 模式不重新编码视频，速度极快但无法改变分辨率/格式。需要精准到帧或改变参数时选择对应编码器。

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
| `←` | 上一帧 |
| `→` | 下一帧 |
| `I` | 标记入点 |
| `O` | 标记出点 |
| `A` | 添加片段到队列 |

## 输出文件

输出保存在源文件同目录：
- 单段剪辑：`原文件名_CLIP_YYYYMMDD_HHMMSS.mp4`
- 多段拼接：`原文件名_CONCAT_YYYYMMDD_HHMMSS.mp4`

## API

所有 API 需要认证（除非 `PASSWORD` 为空）。

| 方法 | 路径 | 说明 |
|------|------|------|
| `POST` | `/api/login` | 登录 |
| `GET` | `/api/auth/check` | 检查认证状态 |
| `POST` | `/api/logout` | 退出登录 |
| `GET` | `/api/gpu` | 检测可用 GPU 编码器 |
| `GET` | `/api/files?path=` | 浏览文件 |
| `GET` | `/api/stream/{path}` | 流式播放视频（支持 Range） |
| `GET` | `/api/info/{path}` | 获取视频元信息（ffprobe） |
| `POST` | `/api/cut` | 单段剪辑 |
| `POST` | `/api/concat` | 多段拼接 |
| `GET` | `/api/tasks` | 获取任务列表 |
| `GET` | `/api/tasks/{id}` | 获取任务状态 |
| `DELETE` | `/api/tasks/{id}` | 取消任务 |

## 技术栈

| 组件 | 技术 |
|------|------|
| 后端 | Rust + Axum 0.8 + Tokio |
| 前端 | Vanilla JS + CSS Custom Properties |
| 视频处理 | FFmpeg 7.x |
| TS 播放 | mpegts.js 1.7.6 |
| 容器 | Debian trixie-slim |
| 构建 | Rust 1.87, multi-stage Dockerfile |
| 二进制大小 | ~12MB |
| 运行内存 | ~2MB |

## License

MIT
