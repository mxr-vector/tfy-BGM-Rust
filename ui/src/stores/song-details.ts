import { shallowRef } from 'vue';
import { state } from './state';

export type SongDetailsTab = 'lyrics' | 'details';

export const songDetailsOpen = shallowRef(false);
export const songDetailsTab = shallowRef<SongDetailsTab>('details');

export function openSongDetails(tab: SongDetailsTab = 'details'): void {
  if (!state.currentSong) return;
  songDetailsTab.value = tab;
  songDetailsOpen.value = true;
}

export function closeSongDetails(): void {
  songDetailsOpen.value = false;
}

export function toggleSongDetails(): void {
  if (!state.currentSong) return;
  songDetailsOpen.value = !songDetailsOpen.value;
}
