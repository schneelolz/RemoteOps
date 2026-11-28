/** 详情与编辑替换同一个弹窗时，保留列表中的原始触发按钮。 */
let origin: HTMLElement | null = null;
export function rememberModalTrigger() {
  const current = document.activeElement as HTMLElement | null;
  if (
    current &&
    current !== document.body &&
    !current.closest('[role="dialog"]')
  )
    origin = current;
}
export function restoreModalTrigger() {
  if (document.querySelector('[role="dialog"]')) return;
  if (origin?.isConnected) origin.focus({ preventScroll: true });
  origin = null;
}
