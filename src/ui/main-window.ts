export function mainWindowShell(): string {
  return `
  <div class="window" id="app-window">
    <header class="titlebar" data-tauri-drag-region>
      <div class="brand">
        <svg class="fox" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.7" aria-hidden="true">
          <path d="M4 5.5 8.2 8 12 6.5 15.8 8 20 5.5l-1.2 8.2C18.3 17.4 15.5 20 12 21c-3.5-1-6.3-3.6-6.8-7.3L4 5.5Z"/>
          <path d="m8.5 13 3.5 2.5 3.5-2.5M12 15.5V21"/>
        </svg>
        <span>KitsuPin</span>
      </div>
      <div class="title-actions">
        <button class="winbtn" id="btn-minimize" aria-label="Свернуть">−</button>
        <button class="winbtn" id="btn-close" aria-label="Закрыть">×</button>
      </div>
    </header>

    <div class="app">
      <aside class="sidebar" id="sidebar">
        <button class="navbtn active" data-screen="history" aria-current="page">
          <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.7" aria-hidden="true"><path d="M4 7h16M4 12h16M4 17h10"/></svg>
          <span>История</span>
        </button>
        <button class="navbtn" data-screen="categories">
          <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.7" aria-hidden="true"><path d="M4 5h7l2 3h7v11H4z"/></svg>
          <span>Категории</span>
        </button>
        <button class="navbtn" data-screen="settings">
          <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.7" aria-hidden="true"><circle cx="12" cy="12" r="3"/><path d="M12 3v2m0 14v2M3 12h2m14 0h2M5.6 5.6 7 7m10 10 1.4 1.4M18.4 5.6 17 7M7 17l-1.4 1.4"/></svg>
          <span>Настройки</span>
        </button>
        <div class="sidebar-foot">
          <div class="rec-status" id="rec-status">
            <i class="dot" id="status-dot"></i>
            <span id="status-text">История записывается</span>
          </div>
        </div>
      </aside>

      <!-- History screen -->
      <main class="screen active" id="history" role="main">
        <div class="toolbar">
          <div class="search">
            <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.7" aria-hidden="true"><circle cx="11" cy="11" r="7"/><path d="m16 16 5 5"/></svg>
            <input id="search" autocomplete="off" placeholder="Поиск по тексту, сайту или категории" aria-label="Поиск"/>
            <span class="kbd">Ctrl F</span>
          </div>
          <button class="btn" id="btn-pause">
            <span>Пауза</span>
          </button>
          <button class="btn" data-action="settings">
            <svg width="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.7" aria-hidden="true"><circle cx="12" cy="12" r="3"/><path d="M12 3v2m0 14v2M3 12h2m14 0h2M5.6 5.6 7 7m10 10 1.4 1.4M18.4 5.6 17 7M7 17l-1.4 1.4"/></svg>
            <span>Настройки</span>
          </button>
        </div>

        <div class="filters-bar" id="filters-container" role="toolbar" aria-label="Фильтры"></div>

        <div class="content" id="content-history">
          <section id="cards" class="clip-list" aria-live="polite" aria-label="Записи буфера обмена"></section>
          <div class="empty" id="no-results" hidden>
            <svg class="fox" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.5" aria-hidden="true">
              <path d="M4 5.5 8.2 8 12 6.5 15.8 8 20 5.5l-1.2 8.2C18.3 17.4 15.5 20 12 21c-3.5-1-6.3-3.6-6.8-7.3L4 5.5Z"/>
            </svg>
            <h2>Лиса ничего не нашла</h2>
            <p>Измените запрос или сбросьте фильтры.</p>
          </div>
        </div>
      </main>

      <!-- Categories screen -->
      <main class="screen" id="categories">
        <div class="pagehead">
          <button class="btn primary" style="float:right" id="btn-new-category">Создать категорию</button>
          <h1>Категории</h1>
          <p class="sub">Небольшие метки для быстрого поиска фрагментов.</p>
        </div>
        <div class="settings-body">
          <div class="panel">
            <div class="category-list" id="category-list"></div>
          </div>
        </div>
      </main>

      <!-- Settings screen -->
      <main class="screen" id="settings">
        <div class="pagehead">
          <h1>Настройки</h1>
          <p class="sub">Поведение истории, горячие клавиши и интеграции.</p>
        </div>
        <div class="settings-layout">
          <nav class="settings-nav">
            <button class="navbtn active" data-tab="general"><span>Основные</span></button>
            <button class="navbtn" data-tab="integration"><span>Интеграция</span></button>
            <button class="navbtn" data-tab="about"><span>О приложении</span></button>
          </nav>
          <div class="settings-body" id="settings-body"></div>
        </div>
      </main>
    </div>
  </div>
  <div id="modal-root"></div>
  `;
}
