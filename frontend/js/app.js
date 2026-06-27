/**
 * NAS Video Editor — Frontend Logic (shadcn-slate theme)
 * Performance-optimized: rAF timeline, cached DOM refs, throttled updates
 */

// =========================================================================
// Global Auth: intercept 401 → redirect to login
// =========================================================================
const _origFetch = window.fetch;
window.fetch = async (...args) => {
    const res = await _origFetch(...args);
    if (res.status === 401 && !window.location.pathname.includes('login')) {
        window.location.href = '/login.html';
        throw new Error('Unauthorized');
    }
    return res;
};

// =========================================================================
// State
// =========================================================================
const state = {
    currentFile: null,
    videoInfo: null,
    inPoint: null,
    outPoint: null,
    segments: [],
    tasks: [],
    isPlaying: false,
    duration: 0,
    dragging: null,
    _currentPath: '',
    mpegtsPlayer: null,
};

// =========================================================================
// Cached DOM References (single query at init)
// =========================================================================
const dom = {};
const $ = id => (dom[id] || (dom[id] = document.getElementById(id)));
const video = $('video-player');

// =========================================================================
// Init
// =========================================================================
document.addEventListener('DOMContentLoaded', async () => {
    // Pre-cache frequently accessed elements
    ['timeline', 'timeline-handle', 'timeline-playhead', 'timeline-buffered',
     'timeline-segments', 'handle-in', 'handle-out', 'time-current', 'time-duration',
     'time-display', 'video-placeholder', 'player-wrapper', 'header-title',
     'video-info', 'icon-play', 'icon-pause', 'icon-vol', 'icon-mute',
     'breadcrumb', 'file-list', 'segments-list', 'segments-empty', 'tasks-list',
     'btn-cut', 'btn-concat', 'sel-codec', 'sel-resolution', 'sel-format',
     'input-extra', 'btn-logout', 'btn-refresh', 'btn-theme', 'btn-play',
     'btn-prev-frame', 'btn-next-frame', 'btn-mark-in', 'btn-mark-out',
     'btn-add-segment', 'btn-clear-segments', 'btn-volume', 'volume-slider',
     'step-size', 'icon-sun', 'icon-moon'].forEach($);

    initTheme();
    bindEvents();

    // Check auth first, then load files
    try {
        const authRes = await fetch('/api/auth/check');
        if (authRes.ok) {
            loadFiles('');
            pollTasks();
            detectGPU();
        }
        // If 401, the global interceptor will redirect to login
    } catch (e) {
        console.warn('Auth check failed:', e);
    }
});

function bindEvents() {
    dom['btn-refresh'].addEventListener('click', () => loadFiles(state._currentPath || ''));
    dom['btn-logout'].addEventListener('click', doLogout);
    dom['btn-theme'].addEventListener('click', toggleTheme);
    video.addEventListener('loadedmetadata', onVideoLoaded);
    video.addEventListener('timeupdate', onTimeUpdate);
    video.addEventListener('play', () => { state.isPlaying = true; updatePlayBtn(); });
    video.addEventListener('pause', () => { state.isPlaying = false; updatePlayBtn(); });
    video.addEventListener('progress', scheduleBufferedUpdate);

    dom['btn-play'].addEventListener('click', togglePlay);
    dom['btn-prev-frame'].addEventListener('click', () => stepFrame(-1));
    dom['btn-next-frame'].addEventListener('click', () => stepFrame(1));
    dom['btn-mark-in'].addEventListener('click', markIn);
    dom['btn-mark-out'].addEventListener('click', markOut);
    dom['btn-add-segment'].addEventListener('click', addSegment);
    dom['btn-clear-segments'].addEventListener('click', clearSegments);
    dom['btn-cut'].addEventListener('click', doCut);
    dom['btn-concat'].addEventListener('click', doConcat);
    dom['btn-volume'].addEventListener('click', toggleMute);
    dom['volume-slider'].addEventListener('input', e => {
        video.volume = e.target.value;
        video.muted = false;
        updateVolumeBtn();
    });

    dom['timeline'].addEventListener('mousedown', onTimelineMouseDown);
    document.addEventListener('mousemove', onTimelineMouseMove);
    document.addEventListener('mouseup', onTimelineMouseUp);
    document.addEventListener('keydown', onKeyDown);

    dom['btn-undo'].addEventListener('click', undo);
}

// =========================================================================
// GPU Detection
// =========================================================================
async function detectGPU() {
    try {
        const res = await fetch('/api/gpu');
        const data = await res.json();
        const sel = dom['sel-codec'];
        for (const opt of sel.options) {
            if (opt.value === 'copy' || opt.value.startsWith('lib')) continue;
            if (!data.available.includes(opt.value)) {
                opt.disabled = true;
                opt.textContent += ' (不可用)';
            }
        }
    } catch (e) { console.warn('GPU detection failed:', e); }
}

// =========================================================================
// File Browser
// =========================================================================
let _browseAbort = null;

async function loadFiles(path) {
    if (_browseAbort) _browseAbort.abort();
    _browseAbort = new AbortController();
    state._currentPath = path;
    try {
        const res = await fetch(`/api/files?path=${encodeURIComponent(path)}`, { signal: _browseAbort.signal });
        const data = await res.json();
        renderBreadcrumb(data.current, data.parent);
        renderFileList(data.dirs, data.files, data.current);
    } catch (e) {
        if (e.name !== 'AbortError') console.error('Load files failed:', e);
    }
}

function renderBreadcrumb(current, parent) {
    const bc = dom['breadcrumb'];
    // Use DocumentFragment for batch DOM update
    const frag = document.createDocumentFragment();
    const addLink = (text, path) => {
        const a = document.createElement('a');
        a.textContent = text;
        a.addEventListener('click', () => loadFiles(path));
        frag.appendChild(a);
    };
    addLink('🏠', '');
    if (current && current !== '/') {
        const parts = current.split('/').filter(Boolean);
        let acc = '';
        for (let i = 0; i < parts.length; i++) {
            acc += (i > 0 ? '/' : '') + parts[i];
            const sep = document.createElement('span');
            sep.className = 'sep';
            sep.textContent = '/';
            frag.appendChild(sep);
            addLink(parts[i], acc);
        }
    }
    bc.textContent = '';
    bc.appendChild(frag);
}

// SVG icon templates (created once, reused)
const ICON_FOLDER = '<svg class="icon" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M22 19a2 2 0 0 1-2 2H4a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2h5l2 3h9a2 2 0 0 1 2 2z"/></svg>';
const ICON_VIDEO = '<svg class="icon" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><polygon points="23 7 16 12 23 17 23 7"/><rect x="1" y="5" width="15" height="14" rx="2" ry="2"/></svg>';

function renderFileList(dirs, files, currentPath) {
    const list = dom['file-list'];
    const frag = document.createDocumentFragment();

    if (currentPath && currentPath !== '/') {
        frag.appendChild(makeFileItem('folder', '..', '', () => {
            const p = currentPath.split('/').filter(Boolean);
            p.pop();
            loadFiles(p.join('/'));
        }));
    }

    for (const d of dirs) {
        frag.appendChild(makeFileItem('folder', d.name, '', () => loadFiles(d.path)));
    }

    for (const f of files) {
        const item = makeFileItem('video', f.name, f.size_fmt, () => loadVideo(f.path, f.name));
        if (state.currentFile === f.path) item.classList.add('active');
        frag.appendChild(item);
    }

    if (!dirs.length && !files.length) {
        const empty = document.createElement('div');
        empty.className = 'file-item';
        empty.style.color = 'var(--muted-foreground)';
        empty.style.cursor = 'default';
        empty.textContent = '暂无视频文件';
        frag.appendChild(empty);
    }

    list.textContent = '';
    list.appendChild(frag);
}

function makeFileItem(type, name, meta, onClick) {
    const div = document.createElement('div');
    div.className = 'file-item';
    div.innerHTML = type === 'folder' ? ICON_FOLDER : ICON_VIDEO;
    const nameSpan = document.createElement('span');
    nameSpan.className = 'name';
    nameSpan.title = name;
    nameSpan.textContent = name;
    div.appendChild(nameSpan);
    if (meta) {
        const metaSpan = document.createElement('span');
        metaSpan.className = 'meta';
        metaSpan.textContent = meta;
        div.appendChild(metaSpan);
    }
    div.addEventListener('click', onClick);
    return div;
}

// =========================================================================
// Video Loading
// =========================================================================
async function loadVideo(path, name) {
    state.currentFile = path;
    state.inPoint = null;
    state.outPoint = null;
    state.isPlaying = false;
    updatePlayBtn();

    if (state.mpegtsPlayer) {
        state.mpegtsPlayer.destroy();
        state.mpegtsPlayer = null;
    }

    dom['video-placeholder'].style.display = 'none';
    dom['player-wrapper'].style.display = 'flex';
    dom['header-title'].textContent = name;

    const streamUrl = `/api/videos/${encodeURIComponent(path)}/stream`;
    const isTs = path.toLowerCase().endsWith('.ts');

    if (isTs && mpegts.isSupported()) {
        const player = mpegts.createPlayer({
            type: 'mpegts',
            isLive: false,
            url: streamUrl,
        }, {
            enableWorker: false,
            lazyLoadMaxDuration: 30,
            lazyLoadRecoverDuration: 10,
            deferLoadAfterSourceOpen: true,
            autoCleanupSourceBuffer: true,
            autoCleanupMaxBackwardDuration: 30,
            autoCleanupMinBackwardDuration: 10,
        });
        state.mpegtsPlayer = player;
        player.attachMediaElement(video);
        player.load();
        player.on(mpegts.Events.ERROR, (e, data) => {
            console.error('mpegts error:', e, data);
            toast('TS 播放出错', 'error');
        });
    } else {
        video.src = streamUrl;
        video.load();
    }

    try {
        const res = await fetch(`/api/videos/${encodeURIComponent(path)}/info`);
        state.videoInfo = await res.json();
        const v = state.videoInfo;
        const parts = [];
        const w = v.video.width, h = v.video.height;
        let resLabel = `${w}×${h}`;
        if (h >= 2160) resLabel += ' 4K';
        else if (h >= 1080) resLabel += ' FHD';
        else if (h >= 720) resLabel += ' HD';
        parts.push(resLabel);
        const cn = (v.video.codec_name || '').toUpperCase();
        const encMap = { H264: 'H.264/AVC', HEVC: 'H.265/HEVC', VP9: 'VP9', AV1: 'AV1', MPEG4: 'MPEG-4' };
        if (cn) parts.push(encMap[cn] || cn);
        if (v.video.fps) {
            const fpsStr = String(v.video.fps);
            if (fpsStr.includes('/')) {
                const [num, den] = fpsStr.split('/').map(Number);
                if (den > 0) parts.push((num / den).toFixed(2) + ' fps');
            } else {
                parts.push(parseFloat(fpsStr).toFixed(2) + ' fps');
            }
        }
        if (v.video.bitrate && v.video.bitrate !== '') {
            const br = parseInt(v.video.bitrate);
            if (!isNaN(br) && br > 0) {
                parts.push(br >= 1000000 ? (br / 1000000).toFixed(1) + ' Mbps' : Math.round(br / 1000) + ' kbps');
            }
        }
        if (v.duration) parts.push(fmtTime(v.duration));
        parts.push(v.size_fmt);
        if (v.audio) {
            const ch = v.audio.channels === 1 ? '单声道' : v.audio.channels === 2 ? '立体声' : (v.audio.channels || '?') + 'ch';
            const sr = v.audio.sample_rate ? (v.audio.sample_rate / 1000).toFixed(1) + 'kHz' : '';
            parts.push((v.audio.codec_name || '').toUpperCase() + ' ' + [ch, sr].filter(Boolean).join(' '));
        }
        if (v.format_name) {
            const fmt = v.format_name.toUpperCase().replace(/,.*/, '');
            parts.push(fmt);
        }
        dom['video-info'].textContent = parts.join(' · ');
    } catch (e) { dom['video-info'].textContent = ''; }
    document.querySelectorAll('.file-item').forEach(el => el.classList.remove('active'));
    // Don't reload file list — just update active state
    const items = dom['file-list'].querySelectorAll('.file-item');
    items.forEach(el => {
        const nameEl = el.querySelector('.name');
        if (nameEl && nameEl.textContent === name) el.classList.add('active');
    });
    updateSegmentsUI();
}

function onVideoLoaded() {
    state.duration = video.duration;
    dom['time-duration'].textContent = fmtTime(video.duration);
    dom['time-current'].textContent = fmtTime(0);
    // Invalidate cached timeline rect
    _timelineRect = null;
    _markerHash = '';
    _lastSegmentHash = '';
    updateTimeline();
    updateHandles();
}

// =========================================================================
// Playback
// =========================================================================
function togglePlay() {
    if (video.paused) video.play(); else video.pause();
}

function updatePlayBtn() {
    dom['icon-play'].style.display = state.isPlaying ? 'none' : 'block';
    dom['icon-pause'].style.display = state.isPlaying ? 'block' : 'none';
}

function stepFrame(dir) {
    const step = parseFloat(dom['step-size'].value);
    video.currentTime = Math.max(0, Math.min(state.duration, video.currentTime + dir * step));
    if (state.isPlaying) video.pause();
}

function toggleMute() {
    video.muted = !video.muted;
    updateVolumeBtn();
}

function updateVolumeBtn() {
    dom['icon-vol'].style.display = video.muted ? 'none' : 'block';
    dom['icon-mute'].style.display = video.muted ? 'block' : 'none';
}

// =========================================================================
// Time Update — rAF-throttled
// =========================================================================
let _timeUpdateRaf = null;
let _lastTimeUpdate = 0;

function onTimeUpdate() {
    // Throttle to ~15fps for UI updates (markers, time text)
    const now = performance.now();
    if (now - _lastTimeUpdate < 66) return; // ~15fps
    _lastTimeUpdate = now;

    dom['time-current'].textContent = fmtTime(video.currentTime);
    updatePlayhead();

    // Update markers (only when state changes)
    renderMarkers();
}

let _markerHash = '';
function renderMarkers() {
    const hash = `${state.inPoint}|${state.outPoint}`;
    if (hash === _markerHash) return;
    _markerHash = hash;

    const el = dom['time-display'];
    el.textContent = '';

    const parts = [];
    if (state.inPoint !== null) parts.push(`In ${fmtTime(state.inPoint)}`);
    if (state.outPoint !== null) parts.push(`Out ${fmtTime(state.outPoint)}`);
    if (state.inPoint !== null && state.outPoint !== null) parts.push(`(${fmtTime(state.outPoint - state.inPoint)})`);
    el.textContent = parts.join(' ');
}

let _bufferedRaf = null;
function scheduleBufferedUpdate() {
    if (_bufferedRaf) return;
    _bufferedRaf = requestAnimationFrame(() => {
        _bufferedRaf = null;
        if (video.buffered.length > 0) {
            const end = video.buffered.end(video.buffered.length - 1);
            dom['timeline-buffered'].style.width = (end / state.duration * 100) + '%';
        }
    });
}

// =========================================================================
// Timeline — GPU-accelerated, cached rect
// =========================================================================
let _timelineRect = null;
let _timelineRectTime = 0;

function getCachedTimelineRect() {
    const now = performance.now();
    // Cache rect for 100ms to avoid layout thrashing
    if (!_timelineRect || now - _timelineRectTime > 100) {
        _timelineRect = dom['timeline'].getBoundingClientRect();
        _timelineRectTime = now;
    }
    return _timelineRect;
}

function updateTimeline() { updatePlayhead(); renderSegmentOverlays(); }

function updatePlayhead() {
    if (!state.duration) return;
    const pct = (video.currentTime / state.duration) * 100;
    dom['timeline-playhead'].style.width = pct + '%';
    const handle = dom['timeline-handle'];
    handle.style.left = pct + '%';
    handle.style.display = state.duration ? 'flex' : 'none';
}

function updateHandles() {
    const hIn = dom['handle-in'], hOut = dom['handle-out'];
    if (state.inPoint !== null) {
        hIn.style.display = 'block';
        hIn.style.left = (state.inPoint / state.duration * 100) + '%';
    } else { hIn.style.display = 'none'; }
    if (state.outPoint !== null) {
        hOut.style.display = 'block';
        hOut.style.left = (state.outPoint / state.duration * 100) + '%';
    } else { hOut.style.display = 'none'; }
}

// Cache segment overlay state to skip redundant renders
let _lastSegmentHash = '';

function renderSegmentOverlays() {
    // Compute hash to skip if unchanged
    const hash = state.segments.map(s => `${s.start}:${s.end}`).join('|') +
        `|${state.inPoint}|${state.outPoint}`;
    if (hash === _lastSegmentHash) return;
    _lastSegmentHash = hash;

    const c = dom['timeline-segments'];
    c.textContent = '';
    const frag = document.createDocumentFragment();

    for (const seg of state.segments) {
        const d = document.createElement('div');
        d.className = 'timeline-segment';
        d.style.left = (seg.start / state.duration * 100) + '%';
        d.style.width = ((seg.end - seg.start) / state.duration * 100) + '%';
        frag.appendChild(d);
    }
    if (state.inPoint !== null && state.outPoint !== null) {
        const d = document.createElement('div');
        d.className = 'timeline-segment';
        d.style.left = (state.inPoint / state.duration * 100) + '%';
        d.style.width = ((state.outPoint - state.inPoint) / state.duration * 100) + '%';
        d.style.background = 'oklch(0.596 0.145 163.225 / 30%)';
        d.style.borderColor = 'var(--success)';
        frag.appendChild(d);
    }
    c.appendChild(frag);
}

function getTimelineTime(e) {
    const rect = getCachedTimelineRect();
    const x = Math.max(0, Math.min(e.clientX - rect.left, rect.width));
    return (x / rect.width) * state.duration;
}

let _dragRaf = null;
let _dragEvent = null;

function onTimelineMouseDown(e) {
    if (!state.duration) return;

    const handle = dom['timeline-handle'];
    const hRect = handle.getBoundingClientRect();
    if (e.clientX >= hRect.left - 4 && e.clientX <= hRect.right + 4 &&
        e.clientY >= hRect.top - 4 && e.clientY <= hRect.bottom + 4) {
        state.dragging = 'playhead';
        if (state.isPlaying) video.pause();
        e.preventDefault();
        return;
    }

    const hIn = dom['handle-in'], hOut = dom['handle-out'];
    if (hIn.style.display !== 'none') {
        const r = hIn.getBoundingClientRect();
        if (e.clientX >= r.left - 2 && e.clientX <= r.right + 2 &&
            e.clientY >= r.top - 2 && e.clientY <= r.bottom + 2) {
            state.dragging = 'in';
            e.preventDefault();
            return;
        }
    }
    if (hOut.style.display !== 'none') {
        const r = hOut.getBoundingClientRect();
        if (e.clientX >= r.left - 2 && e.clientX <= r.right + 2 &&
            e.clientY >= r.top - 2 && e.clientY <= r.bottom + 2) {
            state.dragging = 'out';
            e.preventDefault();
            return;
        }
    }

    video.currentTime = getTimelineTime(e);
    _timelineRect = null; // Invalidate on click
}

function onTimelineMouseMove(e) {
    if (!state.dragging) return;
    _dragEvent = e;
    if (_dragRaf) return;
    _dragRaf = requestAnimationFrame(() => {
        _dragRaf = null;
        const ev = _dragEvent;
        if (!ev) return;
        const time = getTimelineTime(ev);

        if (state.dragging === 'playhead') {
            video.currentTime = time;
        } else if (state.dragging === 'in') {
            state.inPoint = Math.max(0, Math.min(time, state.outPoint ?? state.duration));
        } else if (state.dragging === 'out') {
            state.outPoint = Math.min(state.duration, Math.max(time, state.inPoint ?? 0));
        }

        _lastSegmentHash = ''; // Force re-render during drag
        updateHandles();
        renderSegmentOverlays();
        dom['time-current'].textContent = fmtTime(video.currentTime);
        updatePlayhead();
    });
}

function onTimelineMouseUp() {
    state.dragging = null;
    _timelineRect = null; // Invalidate after drag
}

// =========================================================================
// In/Out Points
// =========================================================================
function markIn() {
    if (!state.duration) return;
    pushHistory({ type: 'setIn', prev: state.inPoint });
    state.inPoint = video.currentTime;
    if (state.outPoint !== null && state.outPoint <= state.inPoint) state.outPoint = null;
    _lastSegmentHash = '';
    updateHandles(); renderSegmentOverlays(); onTimeUpdate();
    toast(`In: ${fmtTime(state.inPoint)}`, 'success');
}

function markOut() {
    if (!state.duration) return;
    pushHistory({ type: 'setOut', prev: state.outPoint });
    state.outPoint = video.currentTime;
    if (state.inPoint !== null && state.inPoint >= state.outPoint) state.inPoint = null;
    _lastSegmentHash = '';
    updateHandles(); renderSegmentOverlays(); onTimeUpdate();
    toast(`Out: ${fmtTime(state.outPoint)}`, 'success');
}

// =========================================================================
// Undo System
// =========================================================================
const _history = [];

function pushHistory(action) {
    _history.push(action);
    if (_history.length > 50) _history.shift();
    updateUndoBtn();
}

function updateUndoBtn() {
    const btn = dom['btn-undo'];
    if (btn) btn.style.display = _history.length ? '' : 'none';
}

function undo() {
    if (!_history.length) return;
    const action = _history.pop();

    switch (action.type) {
        case 'setIn':
            state.inPoint = action.prev;
            toast(action.prev !== null ? `撤回 In: ${fmtTime(action.prev)}` : '撤回入点', 'success');
            break;
        case 'setOut':
            state.outPoint = action.prev;
            toast(action.prev !== null ? `撤回 Out: ${fmtTime(action.prev)}` : '撤回出点', 'success');
            break;
        case 'addSegment':
            state.segments.pop();
            state.inPoint = action.inPoint;
            state.outPoint = action.outPoint;
            toast('撤回添加片段', 'success');
            break;
        case 'removeSegment':
            state.segments.splice(action.index, 0, action.segment);
            toast('撤回删除片段', 'success');
            break;
    }
    _lastSegmentHash = '';
    _markerHash = '';
    updateHandles(); renderSegmentOverlays(); updateSegmentsUI(); onTimeUpdate();
    updateUndoBtn();
}

// =========================================================================
// Segments
// =========================================================================
function addSegment() {
    if (state.inPoint === null || state.outPoint === null) {
        toast('请先标记 In 和 Out 点', 'error'); return;
    }
    pushHistory({ type: 'addSegment', inPoint: state.inPoint, outPoint: state.outPoint });
    state.segments.push({ start: state.inPoint, end: state.outPoint });
    state.inPoint = null;
    state.outPoint = null;
    _lastSegmentHash = '';
    updateHandles(); renderSegmentOverlays(); updateSegmentsUI(); onTimeUpdate();
    toast(`已添加片段 #${state.segments.length}`, 'success');
}

function removeSegment(i) {
    pushHistory({ type: 'removeSegment', index: i, segment: { ...state.segments[i] } });
    state.segments.splice(i, 1);
    _lastSegmentHash = '';
    updateSegmentsUI(); renderSegmentOverlays();
}

function clearSegments() {
    if (!state.segments.length && state.inPoint === null && state.outPoint === null) return;
    pushHistory({
        type: 'clearSegments',
        segments: [...state.segments],
        inPoint: state.inPoint,
        outPoint: state.outPoint,
    });
    state.segments = [];
    state.inPoint = null;
    state.outPoint = null;
    _lastSegmentHash = '';
    updateHandles(); renderSegmentOverlays(); updateSegmentsUI(); onTimeUpdate();
    toast('\u5df2\u6e05\u9664\u6240\u6709\u7247\u6bb5', 'success');
}

function updateSegmentsUI() {
    const list = dom['segments-list'];
    const empty = dom['segments-empty'];

    if (!state.segments.length) {
        list.textContent = '';
        empty.style.display = 'block';
    } else {
        empty.style.display = 'none';
        const frag = document.createDocumentFragment();
        state.segments.forEach((seg, i) => {
            const dur = seg.end - seg.start;
            const div = document.createElement('div');
            div.className = 'segment-item';
            div.innerHTML = `
                <span class="seg-num">${i + 1}</span>
                <span class="seg-time">${fmtTime(seg.start)} → ${fmtTime(seg.end)}</span>
                <span class="seg-dur">${fmtTime(dur)}</span>
                <button class="seg-del" title="删除">
                    <svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><line x1="18" y1="6" x2="6" y2="18"/><line x1="6" y1="6" x2="18" y2="18"/></svg>
                </button>
            `;
            div.querySelector('.seg-del').addEventListener('click', e => { e.stopPropagation(); removeSegment(i); });
            div.addEventListener('click', e => {
                if (e.target.closest('.seg-del')) return;
                video.currentTime = seg.start;
            });
            frag.appendChild(div);
        });
        list.textContent = '';
        list.appendChild(frag);
    }

    dom['btn-cut'].disabled = state.segments.length !== 1;
    dom['btn-concat'].disabled = state.segments.length < 1;
}

// =========================================================================
// FFmpeg Operations
// =========================================================================
function getSettings() {
    return {
        codec: dom['sel-codec'].value,
        resolution: dom['sel-resolution'].value || null,
        format: dom['sel-format'].value || null,
        extra_args: dom['input-extra'].value.trim() || null,
    };
}

async function doCut() {
    if (!state.currentFile || state.segments.length !== 1) return;
    const seg = state.segments[0];
    try {
        const res = await fetch('/api/cut', {
            method: 'POST',
            headers: { 'Content-Type': 'application/json' },
            body: JSON.stringify({ file: state.currentFile, start: seg.start, end: seg.end, ...getSettings() }),
        });
        const data = await res.json();
        if (res.ok) {
            toast(`剪辑任务已提交: ${data.task_id}`, 'success');
            state.segments = []; _lastSegmentHash = ''; updateSegmentsUI(); renderSegmentOverlays(); pollTasks();
        } else { toast(data.detail || '剪辑失败', 'error'); }
    } catch (e) { toast('请求失败: ' + e.message, 'error'); }
}



async function doConcat() {
    if (!state.currentFile || state.segments.length < 1) return;
    try {
        const res = await fetch('/api/concat', {
            method: 'POST',
            headers: { 'Content-Type': 'application/json' },
            body: JSON.stringify({ file: state.currentFile, segments: state.segments, ...getSettings() }),
        });
        const data = await res.json();
        if (res.ok) {
            toast(`拼接任务已提交: ${data.task_id}`, 'success');
            state.segments = []; _lastSegmentHash = ''; updateSegmentsUI(); renderSegmentOverlays(); pollTasks();
        } else { toast(data.detail || '拼接失败', 'error'); }
    } catch (e) { toast('请求失败: ' + e.message, 'error'); }
}

// =========================================================================
// Task Polling — skip re-render if unchanged
// =========================================================================
let _pollTimer = null;
let _lastTasksJSON = '';

async function pollTasks() {
    try {
        const res = await fetch('/api/tasks');
        const tasks = await res.json();
        const json = JSON.stringify(tasks);
        if (json !== _lastTasksJSON) {
            _lastTasksJSON = json;
            state.tasks = tasks;
            renderTasks();
        }
    } catch (e) { console.error('Task poll failed:', e); }

    const hasActive = state.tasks.some(t => t.status === 'queued' || t.status === 'running');
    if (hasActive && !_pollTimer) {
        _pollTimer = setInterval(async () => {
            try {
                const res = await fetch('/api/tasks');
                const tasks = await res.json();
                const json = JSON.stringify(tasks);
                if (json !== _lastTasksJSON) {
                    _lastTasksJSON = json;
                    state.tasks = tasks;
                    renderTasks();
                }
            } catch (e) {}
            if (!state.tasks.some(t => t.status === 'queued' || t.status === 'running')) {
                clearInterval(_pollTimer); _pollTimer = null;
            }
        }, 1000);
    }
}

const TASK_ICON_CUT = '<svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><circle cx="6" cy="6" r="3"/><circle cx="6" cy="18" r="3"/><line x1="20" y1="4" x2="8.12" y2="15.88"/><line x1="14.47" y1="14.48" x2="20" y2="20"/><line x1="8.12" y1="8.12" x2="12" y2="12"/></svg>';
const TASK_ICON_CONCAT = '<svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><line x1="12" y1="5" x2="12" y2="19"/><line x1="5" y1="12" x2="19" y2="12"/></svg>';

function renderTasks() {
    const list = dom['tasks-list'];
    if (!state.tasks.length) {
        const empty = document.createElement('div');
        empty.style.cssText = 'padding:8px 10px;color:var(--muted-foreground);font-size:12px';
        empty.textContent = '\u6682\u65e0\u4efb\u52a1';
        list.textContent = '';
        list.appendChild(empty);
        return;
    }
    const frag = document.createDocumentFragment();
    for (const t of state.tasks) {
        const div = document.createElement('div');
        div.className = 'task-item';

        const header = document.createElement('div');
        header.className = 'task-header';
        const typeSpan = document.createElement('span');
        typeSpan.className = 'task-type';
        typeSpan.innerHTML = (t.type === 'cut' ? TASK_ICON_CUT : TASK_ICON_CONCAT);
        typeSpan.appendChild(document.createTextNode(' ' + (t.type === 'cut' ? 'Cut' : 'Concat') + ' #' + t.id));
        const statusSpan = document.createElement('span');
        statusSpan.className = 'task-status ' + t.status;
        statusSpan.textContent = statusLabel(t.status);
        header.appendChild(typeSpan);
        header.appendChild(statusSpan);
        div.appendChild(header);

        const msgDiv = document.createElement('div');
        msgDiv.className = 'task-msg';
        msgDiv.textContent = t.message || '';
        div.appendChild(msgDiv);

        if (t.status === 'running' || t.status === 'queued') {
            const barOuter = document.createElement('div');
            barOuter.className = 'task-progress-bar';
            const barFill = document.createElement('div');
            barFill.className = 'task-progress-fill';
            barFill.style.width = (t.progress * 100) + '%';
            barOuter.appendChild(barFill);
            div.appendChild(barOuter);

            const actions = document.createElement('div');
            actions.className = 'task-actions';
            const btn = document.createElement('button');
            btn.className = 'btn btn-ghost btn-sm btn-destructive';
            btn.textContent = '\u53d6\u6d88';
            btn.addEventListener('click', async () => {
                await fetch('/api/tasks/' + t.id, { method: 'DELETE' });
                pollTasks();
            });
            actions.appendChild(btn);
            div.appendChild(actions);
        }

        if (t.status === 'completed' && t.output) {
            const actions = document.createElement('div');
            actions.className = 'task-actions';
            const outSpan = document.createElement('span');
            outSpan.className = 'task-output';
            outSpan.textContent = '\ud83d\udcc1 ' + t.output;
            actions.appendChild(outSpan);
            div.appendChild(actions);
        }

        frag.appendChild(div);
    }
    list.textContent = '';
    list.appendChild(frag);
}

function statusLabel(s) {
    return { queued: '等待中', running: '处理中', completed: '完成', failed: '失败', cancelled: '已取消' }[s] || s;
}

// =========================================================================
// Keyboard Shortcuts
// =========================================================================
function onKeyDown(e) {
    if (e.target.tagName === 'INPUT' || e.target.tagName === 'TEXTAREA' || e.target.tagName === 'SELECT') return;
    switch (e.key) {
        case ' ': e.preventDefault(); togglePlay(); break;
        case 'ArrowLeft': e.preventDefault(); stepFrame(-1); break;
        case 'ArrowRight': e.preventDefault(); stepFrame(1); break;
        case 'i': case 'I': e.preventDefault(); markIn(); break;
        case 'o': case 'O': e.preventDefault(); markOut(); break;
        case 'a': case 'A': e.preventDefault(); addSegment(); break;
        case 'z': case 'Z':
            if (e.ctrlKey || e.metaKey) { e.preventDefault(); undo(); }
            break;
    }
}

// =========================================================================
// Utilities
// =========================================================================
function fmtTime(s) {
    if (!s || isNaN(s)) return '00:00:00.000';
    const h = Math.floor(s / 3600);
    const m = Math.floor((s % 3600) / 60);
    const sec = s % 60;
    return `${pad(h)}:${pad(m)}:${sec.toFixed(3).padStart(6, '0')}`;
}

function pad(n) { return String(Math.floor(n)).padStart(2, '0'); }

function toast(msg, type = 'info') {
    const el = document.createElement('div');
    el.className = `toast ${type}`;
    el.textContent = msg;
    document.body.appendChild(el);
    setTimeout(() => el.remove(), 3000);
}

// =========================================================================
// Theme
// =========================================================================
function initTheme() {
    const saved = localStorage.getItem('theme') || 'dark';
    setTheme(saved);
}

function toggleTheme() {
    const current = document.documentElement.getAttribute('data-theme') || 'dark';
    setTheme(current === 'dark' ? 'light' : 'dark');
}

function setTheme(theme) {
    document.documentElement.setAttribute('data-theme', theme);
    localStorage.setItem('theme', theme);
    dom['icon-sun'].style.display = theme === 'dark' ? 'none' : 'block';
    dom['icon-moon'].style.display = theme === 'dark' ? 'block' : 'none';
}

// =========================================================================
// Logout
// =========================================================================
async function doLogout() {
    try { await fetch('/api/logout', { method: 'POST' }); } catch {}
    window.location.href = '/login.html';
}
