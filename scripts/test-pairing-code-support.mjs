// 只解析夹具的控制码字段或明确标记的控制台输出，原值只留内存。
export function shortCode(value) {
  const compact = (value ?? '').replace(/[ -]/g, '');
  return /^[A-Za-z0-9]{9}$/.test(compact) && /[0-9]/.test(compact) ? compact.toUpperCase() : undefined;
}

export function consoleCode(output) {
  const match = /控制码[：:][ \t]*([A-Za-z0-9]{3}[- ]?[A-Za-z0-9]{3}[- ]?[A-Za-z0-9]{3})(?![A-Za-z0-9-])/.exec(output);
  return shortCode(match?.[1]);
}
