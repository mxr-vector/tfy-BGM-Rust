import { shallowRef } from 'vue';
import { state } from './state';

export type SongDetailsTab = 'lyrics' | 'details';

export const songDetailsOpen = shallowRef(false);
export const songDetailsTab = shallowRef<SongDetailsTab>('details');

export function closeSongDetails(): void {
  songDetailsOpen.value = false;
}

export function toggleSongDetails(): void {
  if (!state.currentSong) return;
  songDetailsOpen.value = !songDetailsOpen.value;
}
