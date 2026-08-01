export function popupShell(): string {
  return `<main class="popup">
    <header class="popup-head"><div class="brand-mark">K</div><div class="search"><i data-lucide="search"></i><input id="search" autocomplete="off" placeholder="Найти в KitsuPin…" aria-label="Поиск"/></div></header>
    <div id="filters-container"></div>
    <section id="cards" class="popup-list" aria-live="polite"></section>
    <footer><span>↑↓ выбрать · Enter скопировать</span><kbd>Esc</kbd></footer>
  </main>`;
}
