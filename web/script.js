// 使用IIFE（立即执行函数表达式）创建私有作用域，避免全局变量污染
(function() {
    'use strict';

    // ============ DOM 元素缓存 ============
    const DOM = {
        form: document.getElementById('github-form'),
        input: document.getElementById('githubLinkInput'),
        output: document.getElementById('formattedLinkOutput'),
        outputArea: document.getElementById('output'),
        copyButton: document.getElementById('copyButton'),
        openButton: document.getElementById('openButton'),
        toast: document.getElementById('toast'),
        error: document.getElementById('githubLinkError'),
        formatToggle: document.getElementById('format-toggle'),
        slider: document.querySelector('.segmented-control__slider'),
        clearButton: document.getElementById('clearInputButton'),
        versionBadge: document.getElementById('versionBadge'),
    };

    // ============ 常量配置 ============
    const CONFIG = {
        TOAST_DURATION: 3000,
        API_CONFIG: '/api/config',
        API_HEALTH: '/healthz',
        DEBOUNCE_DELAY: 300,
        THROTTLE_DELAY: 16, // ~60fps
    };

    // ============ 高阶函数 ============
    /**
     * 防抖函数：延迟执行，如果在延迟期间再次调用，则重新计时
     * 适用于：输入框输入、窗口调整大小、搜索建议等
     * @param {Function} func - 要执行的函数
     * @param {number} delay - 延迟毫秒数
     * @returns {Function} - 防抖后的函数
     */
    function debounce(func, delay) {
        let timeoutId;
        return function debounced(...args) {
            clearTimeout(timeoutId);
            timeoutId = setTimeout(() => func.apply(this, args), delay);
        };
    }

    /**
     * 节流函数：在指定时间间隔内最多执行一次
     * 适用于：resize、scroll、mousemove 等高频事件
     * @param {Function} func - 要执行的函数
     * @param {number} delay - 时间间隔毫秒数
     * @returns {Function} - 节流后的函数
     */
    function throttle(func, delay) {
        let lastTime = 0;
        return function throttled(...args) {
            const now = Date.now();
            if (now - lastTime >= delay) {
                func.apply(this, args);
                lastTime = now;
            }
        };
    }

    // ============ Toast 管理系统 ============
    const ToastManager = {
        queue: [],
        isShowing: false,
        lastMessage: null,
        lastTime: 0,
        timeoutId: null,

        /**
         * 显示 Toast 消息（带去重功能）
         * @param {string} message - 提示消息
         * @param {number} minInterval - 相同消息的最小显示间隔（ms）
         */
        show(message, minInterval = 1000) {
            const now = Date.now();
            
            // 去重：相同消息且在最小间隔内不重复显示
            if (message === this.lastMessage && now - this.lastTime < minInterval) {
                return;
            }

            this.queue.push(message);
            this.lastMessage = message;
            this.lastTime = now;

            if (!this.isShowing) {
                this.processQueue();
            }
        },

        /**
         * 处理 Toast 队列
         */
        processQueue() {
            if (this.queue.length === 0) {
                this.isShowing = false;
                return;
            }

            this.isShowing = true;
            const message = this.queue.shift();
            this.display(message);
        },

        /**
         * 显示单个 Toast
         */
        display(message) {
            const toastMessage = DOM.toast.querySelector('#toastMessage');
            toastMessage.textContent = message;
            DOM.toast.classList.add('toast--visible');

            // 清除之前的计时器
            if (this.timeoutId) {
                clearTimeout(this.timeoutId);
            }

            // 设置新的计时器
            this.timeoutId = setTimeout(() => {
                DOM.toast.classList.remove('toast--visible');
                // 继续处理队列中的下一个消息
                this.processQueue();
            }, CONFIG.TOAST_DURATION);
        },

        /**
         * 清空队列
         */
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
     * 显示吐司提示（使用 ToastManager）
     * @param {string} message - 提示消息
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
     * 生成输出链接
     * @param {string} userInput - 用户输入的URL
     * @param {string} format - 输出格式
     * @returns {object} - { link, isUrl } 或 { error }
     */
    function generateOutput(userInput, format) {
        const baseUrl = window.location.origin;
        let normalizedLink = userInput.trim();

        try {
            if (!/^https?:\/\//i.test(normalizedLink)) {
                normalizedLink = 'https://' + normalizedLink;
            }

            const url = new URL(normalizedLink);
            const proxyPath = url.hostname + url.pathname + url.search + url.hash;
            const directLink = `${baseUrl}/${proxyPath}`;

            switch (format) {
                case 'git':
                    if (url.pathname.endsWith('.git')) {
                        return { link: `git clone ${directLink}`, isUrl: false };
                    }
                    return { error: 'Git Clone 需要以 .git 结尾的仓库链接' };
                case 'wget':
                    return { link: `wget "${directLink}"`, isUrl: false };
                case 'curl':
                    return { link: `curl -O "${directLink}"`, isUrl: false };
                case 'direct':
                default:
                    return { link: directLink, isUrl: true };
            }
        } catch (e) {
            return { error: '请输入一个有效的 URL' };
        }
    }

    /**
     * 处理表单提交
     */
    function handleFormAction() {
        DOM.error.textContent = '';
        DOM.error.classList.remove('text-field__error--visible');

        const githubLink = DOM.input.value.trim();
        const selectedFormat = DOM.formatToggle.querySelector('.active').dataset.value;

        if (!githubLink) {
            DOM.error.textContent = '请输入Github链接';
            DOM.error.classList.add('text-field__error--visible');
            return;
        }

        const result = generateOutput(githubLink, selectedFormat);

        if (result.error) {
            DOM.error.textContent = result.error;
            DOM.error.classList.add('text-field__error--visible');
            DOM.outputArea.style.display = 'none';
        } else {
            DOM.output.textContent = result.link;
            DOM.outputArea.style.display = 'flex';
            DOM.openButton.disabled = !result.isUrl;
        }
    }

    /**
     * 更新滑块位置
     */
    function updateSliderPosition() {
        const activeButton = DOM.formatToggle.querySelector('.active');
        if (activeButton) {
            const rect = activeButton.getBoundingClientRect();
            const containerRect = DOM.formatToggle.getBoundingClientRect();
            DOM.slider.style.width = `${rect.width}px`;
            DOM.slider.style.transform = `translateX(${rect.left - containerRect.left}px)`;
        }
    }

    /**
     * 初始化滑块和响应式观察
     */
    function initSlider() {
        updateSliderPosition();
        // 使用节流优化 ResizeObserver 回调频率
        const throttledUpdateSlider = throttle(updateSliderPosition, CONFIG.THROTTLE_DELAY);
        const resizeObserver = new ResizeObserver(throttledUpdateSlider);
        resizeObserver.observe(DOM.formatToggle);
    }

    /**
     * 使用 execCommand 复制文本（降级方案）
     * @param {string} text - 要复制的文本
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
                console.warn('execCommand 返回 false，尝试备用方案');
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
     * 复制链接到剪贴板
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
     * 在新标签页打开链接
     */
    function openInNewTab() {
        if (!DOM.openButton.disabled) {
            window.open(DOM.output.textContent, '_blank');
        }
    }

    /**
     * 获取配置数据并更新UI
     */
    async function fetchConfigData() {
        try {
            const response = await fetch(CONFIG.API_CONFIG);
            if (!response.ok) throw new Error(`HTTP error! Status: ${response.status}`);
            const config = await response.json();

            const updates = {
                'sizeLimitDisplay': config.server ? `${config.server.sizeLimit} MB` : '无法获取',
                'blackListStatus': config.blacklist ? (config.blacklist.enabled ? '已开启' : '已关闭') : '无法获取',
                'shellNestStatus': config.shell ? (config.shell.editor ? '已开启' : '已关闭') : '无法获取',
            };

            Object.entries(updates).forEach(([id, text]) => {
                const element = document.getElementById(id);
                if (element) element.textContent = text;
            });
        } catch (error) {
            console.error('Error fetching config:', error);
            ['sizeLimitDisplay', 'blackListStatus', 'shellNestStatus'].forEach(id => {
                const element = document.getElementById(id);
                if (element) element.textContent = '无法获取';
            });
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
            console.warn('无法获取 /healthz 版本信息:', err);
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

        DOM.formatToggle.addEventListener('click', (e) => {
            const button = e.target.closest('button');
            if (!button || button.classList.contains('active')) return;
            DOM.formatToggle.querySelector('.active')?.classList.remove('active');
            button.classList.add('active');
            updateSliderPosition();
            if (DOM.input.value.trim()) {
                handleFormAction();
            }
        });

        // 使用防抖处理输入事件，避免频繁触发
        const debouncedInputHandler = debounce(() => {
            DOM.error.textContent = '';
            DOM.error.classList.remove('text-field__error--visible');
            updateClearButtonVisibility();
        }, CONFIG.DEBOUNCE_DELAY);

        DOM.input.addEventListener('input', debouncedInputHandler);

        // 使用节流处理窗口调整大小时的滑块位置更新
        const throttledSliderUpdate = throttle(updateSliderPosition, CONFIG.THROTTLE_DELAY);
        window.addEventListener('resize', throttledSliderUpdate);

        DOM.copyButton.addEventListener('click', copyToClipboard);
        DOM.openButton.addEventListener('click', openInNewTab);
    }

    // ============ 初始化 ============
    function init() {
        setupEventListeners();
        fetchConfigData();
        initSlider();
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