export function popupShell(filters: string): string {
  return `<main class="popup">
    <header class="popup-head"><div class="brand-mark">P</div><div class="search"><i data-lucide="search"></i><input id="search" autocomplete="off" placeholder="Найти в Pastily…" aria-label="Поиск"/></div></header>
    ${filters}
    <section id="cards" class="popup-list" aria-live="polite"></section>
    <footer><span>↑↓ выбрать · Enter скопировать</span><kbd>Esc</kbd></footer>
  </main>`;
}
