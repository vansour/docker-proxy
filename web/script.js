// 使用IIFE（立即执行函数表达式）创建私有作用域，避免全局变量污染
(function() {
    'use strict';

    // ============ DOM 元素缓存 ============
    const DOM = {
        form: document.getElementById('docker-form'),
        input: document.getElementById('dockerImageInput'),
        output: document.getElementById('formattedOutput'),
        outputArea: document.getElementById('output'),
        copyButton: document.getElementById('copyButton'),
        toast: document.getElementById('toast'),
        error: document.getElementById('dockerImageError'),
        clearButton: document.getElementById('clearInputButton'),
        versionBadge: document.getElementById('versionBadge'),
    };

    // ============ 常量配置 ============
    const CONFIG = {
        TOAST_DURATION: 3000,
        API_HEALTH: '/healthz',
        DEBOUNCE_DELAY: 300,
    };

    // ============ 高阶函数 ============
    /**
     * 防抖函数：延迟执行，如果在延迟期间再次调用，则重新计时
     */
    function debounce(func, delay) {
        let timeoutId;
        return function debounced(...args) {
            clearTimeout(timeoutId);
            timeoutId = setTimeout(() => func.apply(this, args), delay);
        };
    }

    // ============ Toast 管理系统 ============
    const ToastManager = {
        queue: [],
        isShowing: false,
        timeoutId: null,

        show(message) {
            this.queue.push(message);
            if (!this.isShowing) {
                this.processQueue();
            }
        },

        processQueue() {
            if (this.queue.length === 0) {
                this.isShowing = false;
                return;
            }

            this.isShowing = true;
            const message = this.queue.shift();
            this.display(message);
        },

        display(message) {
            const toastMessage = DOM.toast.querySelector('#toastMessage');
            toastMessage.textContent = message;
            DOM.toast.classList.add('toast--visible');

            if (this.timeoutId) {
                clearTimeout(this.timeoutId);
            }

            this.timeoutId = setTimeout(() => {
                DOM.toast.classList.remove('toast--visible');
                this.processQueue();
            }, CONFIG.TOAST_DURATION);
        },

        clear() {
            this.queue = [];
            this.isShowing = false;
            if (this.timeoutId) {
                clearTimeout(this.timeoutId);
            }
            DOM.toast.classList.remove('toast--visible');
        }
    };

    /**
     * 显示吐司提示
     */
    function showToast(message) {
        ToastManager.show(message);
    }

    /**
     * 更新清除按钮的可见性
     */
    function updateClearButtonVisibility() {
        const hasInput = DOM.input.value.trim().length > 0;
        DOM.clearButton.classList.toggle('visible', hasInput);
    }

    /**
     * 解析 Docker 镜像名称
     * @param {string} imageName - 镜像名称
     * @returns {object} - { registry, name, tag }
     */
    function parseDockerImage(imageName) {
        imageName = imageName.trim();
        if (!imageName) return null;

        let registry = 'docker.io';
        let name = imageName;
        let tag = 'latest';

        // 检查是否包含 tag
        const tagIndex = name.lastIndexOf(':');
        if (tagIndex !== -1 && !name.includes('/') || (tagIndex > name.lastIndexOf('/'))) {
            tag = name.substring(tagIndex + 1);
            name = name.substring(0, tagIndex);
        }

        // 检查是否包含 registry
        if (name.includes('/')) {
            const parts = name.split('/');
            if (parts[0].includes('.') || parts[0].includes(':')) {
                registry = parts[0];
                name = parts.slice(1).join('/');
            }
        } else {
            // 如果没有 slash，添加 library 前缀
            name = 'library/' + name;
        }

        return { registry, name, tag };
    }

    /**
     * 生成 docker pull 命令
     */
    function generateDockerPullCommand(imageName) {
        const parsed = parseDockerImage(imageName);
        if (!parsed) {
            return { error: '请输入有效的镜像名称' };
        }

        const proxyHost = window.location.host;
        const fullImage = `${parsed.name}:${parsed.tag}`;
        const command = `docker pull ${proxyHost}/${fullImage}`;

        return { command, registry: parsed.registry, image: fullImage };
    }

    /**
     * 处理表单提交
     */
    function handleFormAction() {
        DOM.error.textContent = '';
        DOM.error.classList.remove('text-field__error--visible');

        const imageName = DOM.input.value.trim();

        if (!imageName) {
            DOM.error.textContent = '请输入镜像名称';
            DOM.error.classList.add('text-field__error--visible');
            return;
        }

        const result = generateDockerPullCommand(imageName);

        if (result.error) {
            DOM.error.textContent = result.error;
            DOM.error.classList.add('text-field__error--visible');
            DOM.outputArea.style.display = 'none';
        } else {
            DOM.output.textContent = result.command;
            DOM.outputArea.style.display = 'flex';
        }
    }

    /**
     * 使用 execCommand 复制文本（降级方案）
     */
    function copyViaExecCommand(text) {
        const textarea = document.createElement('textarea');
        Object.assign(textarea.style, {
            position: 'fixed',
            top: '0',
            left: '0',
            width: '2em',
            height: '2em',
            padding: '0',
            border: 'none',
            outline: 'none',
            boxShadow: 'none',
            background: 'transparent',
            opacity: '0',
            pointerEvents: 'none',
            zIndex: '-9999',
            fontSize: '16px',
            lineHeight: '1',
        });

        textarea.value = text;
        document.body.appendChild(textarea);
        textarea.focus();

        try {
            textarea.setSelectionRange(0, textarea.value.length);
            const successful = document.execCommand('copy');

            if (successful) {
                showToast('已复制到剪贴板');
            } else {
                textarea.select();
                const retrySuccessful = document.execCommand('copy');
                showToast(retrySuccessful ? '已复制到剪贴板' : '复制失败');
            }
        } catch (err) {
            console.error('复制错误:', err);
            showToast('复制失败，请手动复制');
        } finally {
            if (textarea.parentNode) {
                document.body.removeChild(textarea);
            }
        }
    }

    /**
     * 复制命令到剪贴板
     */
    function copyToClipboard() {
        const textToCopy = DOM.output.textContent;
        if (!textToCopy) return;

        if (navigator.clipboard?.writeText) {
            navigator.clipboard.writeText(textToCopy)
                .then(() => showToast('已复制到剪贴板'))
                .catch(() => copyViaExecCommand(textToCopy));
        } else {
            copyViaExecCommand(textToCopy);
        }
    }

    /**
     * 获取版本信息并更新右下角角标
     */
    async function fetchHealthVersion() {
        if (!DOM.versionBadge) return;
        try {
            const resp = await fetch(CONFIG.API_HEALTH, { cache: 'no-store' });
            if (!resp.ok) throw new Error(`status ${resp.status}`);
            const data = await resp.json();
            if (data?.version) {
                const ver = String(data.version).startsWith('v') ? data.version : `v${data.version}`;
                DOM.versionBadge.textContent = ver;
                DOM.versionBadge.title = `版本 ${ver}`;
            } else {
                throw new Error('No version in response');
            }
        } catch (err) {
            console.warn('无法获取版本信息:', err);
            DOM.versionBadge.textContent = 'v?';
            DOM.versionBadge.title = '版本：获取失败';
        }
    }

    // ============ 事件监听器 ============
    function setupEventListeners() {
        DOM.form.addEventListener('submit', (e) => {
            e.preventDefault();
            handleFormAction();
        });

        DOM.clearButton.addEventListener('click', (e) => {
            e.preventDefault();
            DOM.input.value = '';
            updateClearButtonVisibility();
            DOM.error.textContent = '';
            DOM.error.classList.remove('text-field__error--visible');
            DOM.outputArea.style.display = 'none';
            DOM.input.focus();
        });

        const debouncedInputHandler = debounce(() => {
            DOM.error.textContent = '';
            DOM.error.classList.remove('text-field__error--visible');
            updateClearButtonVisibility();
        }, CONFIG.DEBOUNCE_DELAY);

        DOM.input.addEventListener('input', debouncedInputHandler);
        DOM.copyButton.addEventListener('click', copyToClipboard);
    }

    // ============ 初始化 ============
    function init() {
        setupEventListeners();
        updateClearButtonVisibility();
        fetchHealthVersion();
    }

    // 页面加载完成后初始化
    if (document.readyState === 'loading') {
        document.addEventListener('DOMContentLoaded', init);
    } else {
        init();
    }
})();
