export function popupShell(): string {
  return `
  <div class="popup" role="dialog" aria-label="История буфера обмена">
    <div class="popup-search">
      <svg width="20" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.7" aria-hidden="true">
        <circle cx="11" cy="11" r="7"/><path d="m16 16 5 5"/>
      </svg>
      <input id="search" autocomplete="off" placeholder="Найти и скопировать…" aria-label="Поиск" role="combobox" aria-autocomplete="list" aria-controls="cards" aria-expanded="true" aria-haspopup="listbox"/>
      <span class="kbd">Esc</span>
    </div>
    <div id="filters-container" class="popup-filters" role="toolbar" aria-label="Фильтры"></div>
    <section id="cards" class="popup-list" role="listbox" aria-label="Результаты поиска"></section>
    <div id="sr-announcer" class="sr-only" aria-live="polite" aria-atomic="true" style="position:absolute;width:1px;height:1px;padding:0;margin:-1px;overflow:hidden;clip:rect(0,0,0,0);white-space:nowrap;border:0;"></div>
    <div class="popup-foot">
      <span><span class="kbd">↑↓</span> выбрать · <span class="kbd">Enter</span> скопировать</span>
      <span>KitsuPin · локально</span>
    </div>
  </div>`;
}
