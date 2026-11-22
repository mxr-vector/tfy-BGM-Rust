import { ref } from 'vue';
import { state } from './state';

export const diagnosticsOpen = ref(false);

export function openDiagnostics(): void {
  if (!state.currentSong) return;
  diagnosticsOpen.value = true;
}

export function closeDiagnostics(): void {
  diagnosticsOpen.value = false;
}
