const formatter=new Intl.RelativeTimeFormat("ru",{numeric:"auto"});
export function relativeTime(timestamp: number | string, now = Date.now()) {
  const timeMs = typeof timestamp === "number" ? timestamp : new Date(timestamp).getTime();
  const seconds = Math.round((timeMs - now) / 1000);
  const units: [Intl.RelativeTimeFormatUnit, number][] = [["year", 31536000], ["month", 2592000], ["day", 86400], ["hour", 3600], ["minute", 60]];
  for (const [unit, size] of units) {
    if (Math.abs(seconds) >= size || unit === "minute") return formatter.format(Math.round(seconds / size), unit);
  }
  return "сейчас";
}
