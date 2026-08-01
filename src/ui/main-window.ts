export function mainWindowShell(): string {
  return `<div class="top-wash"></div>
    <header class="masthead">
      <div class="brand"><div class="brand-mark">K</div><div><h1>KitsuPin</h1><p>ваши мысли, под рукой</p></div></div>
      <div class="search wide"><i data-lucide="search"></i><input id="search" autocomplete="off" placeholder="Поиск по истории, источникам и категориям…" aria-label="Поиск"/></div>
      <button class="icon-button" data-action="settings" aria-label="Настройки"><i data-lucide="settings"></i></button>
    </header>
    <main>
      <div class="section-lead">
        <div><p class="eyebrow">БУФЕР ОБМЕНА</p><h2>Недавние фрагменты</h2></div>
        <div class="group-control"><label for="grouping">Группировать по</label><select id="grouping"><option value="none">Без группировки</option><option value="domain">Домену</option><option value="category">Категории</option><option value="type">Системному типу</option></select></div>
      </div>
      <div id="filters-container"></div>
      <section id="cards" class="card-board" aria-live="polite"></section>
    </main>
    <div id="modal-root"></div>`;
}
