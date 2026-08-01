export function popupShell(): string {
  return `
  <div class="popup" role="dialog" aria-label="История буфера обмена">
    <div class="popup-search">
      <svg width="20" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.7" aria-hidden="true">
        <circle cx="11" cy="11" r="7"/><path d="m16 16 5 5"/>
      </svg>
      <input id="search" autocomplete="off" placeholder="Найти и скопировать…" aria-label="Поиск"/>
      <span class="kbd">Esc</span>
    </div>
    <div id="filters-container" class="popup-filters" role="toolbar" aria-label="Фильтры"></div>
    <section id="cards" class="popup-list" aria-live="polite"></section>
    <div class="popup-foot">
      <span><span class="kbd">↑↓</span> выбрать · <span class="kbd">Enter</span> скопировать</span>
      <span>KitsuPin · локально</span>
    </div>
  </div>`;
}
