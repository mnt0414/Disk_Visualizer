import { open } from '@tauri-apps/plugin-dialog'

export async function chooseFolder(): Promise<string | null> {
  if (!('__TAURI_INTERNALS__' in window)) return null
  const selection = await open({ directory: true, multiple: false })
  return typeof selection === 'string' ? selection : null
}
