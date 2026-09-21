import { initI18n, t, getLocale, onLocaleChange } from './i18n.js';

const $ = (sel, scope = document) => scope.querySelector(sel);
const $$ = (sel, scope = document) => [...scope.querySelectorAll(sel)];

let allPosts = [];

function formatDate(dateStr) {
  const d = new Date(dateStr);
  const locale = getLocale() === 'en' ? 'en-US' : 'zh-CN';
  return d.toLocaleDateString(locale, { year: 'numeric', month: 'long', day: 'numeric' });
}

function renderMarkdown(text) {
  // 处理代码块
  text = text.replace(/```(\w*)\n([\s\S]*?)```/g, (_, lang, code) => {
    return `<pre><code class="language-${lang}">${escapeHtml(code.trim())}</code></pre>`;
  });
  
  // 处理标题
  text = text.replace(/^### (.+)$/gm, '<h4>$1</h4>');
  text = text.replace(/^## (.+)$/gm, '<h3>$1</h3>');
  text = text.replace(/^# (.+)$/gm, '<h2>$1</h2>');
  
  // 处理无序列表
  text = text.replace(/^- (.+)$/gm, '<li>$1</li>');
  text = text.replace(/(<li>.*<\/li>\n?)+/g, '<ul>$&</ul>');
  
  // 处理加粗
  text = text.replace(/\*\*(.+?)\*\*/g, '<strong>$1</strong>');
  
  // 处理行内代码
  text = text.replace(/`([^`]+)`/g, '<code>$1</code>');
  
  // 处理段落
  text = text.replace(/\n\n/g, '</p><p>');
  text = text.replace(/^/, '<p>');
  text = text.replace(/$/, '</p>');
  
  // 清理空段落
  text = text.replace(/<p><\/p>/g, '');
  text = text.replace(/<p>(<h[234]>)/g, '$1');
  text = text.replace(/(<\/h[234]>)<\/p>/g, '$1');
  text = text.replace(/<p>(<pre>)/g, '$1');
  text = text.replace(/(<\/pre>)<\/p>/g, '$1');
  text = text.replace(/<p>(<ul>)/g, '$1');
  text = text.replace(/(<\/ul>)<\/p>/g, '$1');
  
  return text;
}

function escapeHtml(text) {
  const div = document.createElement('div');
  div.textContent = text;
  return div.innerHTML;
}

function getLocalizedField(post, field) {
  const locale = getLocale();
  const value = post[field];
  if (typeof value === 'object' && !Array.isArray(value)) {
    return value[locale] || value['zh'] || '';
  }
  return value;
}

function renderPostList(posts) {
  const list = $('[data-blog-list]');
  const empty = $('[data-blog-empty]');
  const detail = $('[data-blog-detail]');

  if (!posts.length) {
    list.style.display = 'none';
    empty.hidden = false;
    detail.hidden = true;
    return;
  }

  empty.hidden = true;
  detail.hidden = true;
  list.style.display = '';
  
  list.innerHTML = posts.map(post => {
    const title = getLocalizedField(post, 'title');
    const summary = getLocalizedField(post, 'summary');
    const highlights = getLocalizedField(post, 'highlights') || [];

    return `
    <article class="blog-card" data-post-id="${post.id}">
      <div class="blog-card-header">
        <span class="blog-tag">${post.tag}</span>
        <time class="blog-date" datetime="${post.date}">${formatDate(post.date)}</time>
      </div>
      <h2 class="blog-title">${title}</h2>
      <p class="blog-summary">${summary}</p>
      ${highlights.length ? `
      <div class="blog-highlights">
        ${highlights.map(h => `<span class="blog-highlight-tag">${h}</span>`).join('')}
      </div>` : ''}
      <button class="blog-read-more" data-post-id="${post.id}">
        ${t('blog.readMore') || '阅读全文'} <span aria-hidden="true">→</span>
      </button>
    </article>
  `;
  }).join('');

  // 绑定点击事件
  $$('[data-post-id]', list).forEach(btn => {
    btn.addEventListener('click', () => {
      const postId = btn.dataset.postId;
      showPostDetail(postId);
    });
  });
}

function showPostDetail(postId) {
  const post = allPosts.find(p => p.id === postId);
  if (!post) return;

  const list = $('[data-blog-list]');
  const detail = $('[data-blog-detail]');
  
  const title = getLocalizedField(post, 'title');
  const summary = getLocalizedField(post, 'summary');
  const body = getLocalizedField(post, 'body') || [];
  const highlights = getLocalizedField(post, 'highlights') || [];

  list.style.display = 'none';
  detail.hidden = false;
  
  detail.innerHTML = `
    <button class="blog-back" data-blog-back>
      <span aria-hidden="true">←</span> ${t('blog.backToList') || '返回列表'}
    </button>
    <article class="blog-detail-card">
      <div class="blog-card-header">
        <span class="blog-tag">${post.tag}</span>
        <time class="blog-date" datetime="${post.date}">${formatDate(post.date)}</time>
      </div>
      <h1 class="blog-detail-title">${title}</h1>
      <p class="blog-summary">${summary}</p>
      ${highlights.length ? `
      <div class="blog-highlights">
        ${highlights.map(h => `<span class="blog-highlight-tag">${h}</span>`).join('')}
      </div>` : ''}
      <div class="blog-body">
        ${body.map(renderMarkdown).join('')}
      </div>
    </article>
  `;

  // 绑定返回按钮
  $('[data-blog-back]', detail).addEventListener('click', () => {
    hidePostDetail();
  });

  // 滚动到顶部
  window.scrollTo({ top: 0, behavior: 'smooth' });
  
  // 更新 URL hash
  history.pushState(null, '', `#${postId}`);
}

function hidePostDetail() {
  const list = $('[data-blog-list]');
  const detail = $('[data-blog-detail]');
  
  detail.hidden = true;
  list.style.display = '';
  
  // 清除 URL hash
  history.pushState(null, '', window.location.pathname);
}

async function loadPosts() {
  const posts = [];
  
  // Load main blog.json (release notes)
  try {
    const response = await fetch('./data/blog.json');
    const releasePosts = await response.json();
    posts.push(...releasePosts);
  } catch (err) {
    console.error('Failed to load blog.json:', err);
  }
  
  // Load deep-dive articles from blog-posts/
  const postFiles = [
    'architecture.json',
    'plugin-abi.json',
    'agent-loop.json',
    'execution-env.json'
  ];
  
  for (const file of postFiles) {
    try {
      const response = await fetch(`./data/blog-posts/${file}`);
      const post = await response.json();
      posts.push(post);
    } catch (err) {
      console.error(`Failed to load ${file}:`, err);
    }
  }
  
  // Sort by date (newest first)
  posts.sort((a, b) => new Date(b.date) - new Date(a.date));
  
  return posts;
}

async function init() {
  await initI18n();
  allPosts = await loadPosts();
  renderPostList(allPosts);
  
  // 检查 URL hash，如果有则显示对应文章
  const hash = window.location.hash.slice(1);
  if (hash && allPosts.find(p => p.id === hash)) {
    showPostDetail(hash);
  }
  
  // 监听语言切换
  onLocaleChange(() => {
    const currentHash = window.location.hash.slice(1);
    if (currentHash && allPosts.find(p => p.id === currentHash)) {
      showPostDetail(currentHash);
    } else {
      renderPostList(allPosts);
    }
  });
}

init();
