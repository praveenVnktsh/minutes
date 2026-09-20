import { useState, useEffect, useRef } from 'react';
import { invoke } from '@tauri-apps/api/core';

// Clamps a requested playback position. While `duration` is unknown (0, or
// not finite - the file hasn't finished decoding yet) the request is passed
// through unchanged rather than collapsed to 0, so a click on a transcript
// line made before the audio is ready isn't silently lost.
export function clampSeekTime(time: number, duration: number): number {
  if (!Number.isFinite(time) || time < 0) return 0;
  if (Number.isFinite(duration) && duration > 0) {
    return Math.min(time, duration);
  }
  return time;
}

export const useAudioPlayer = (audioPath: string | null) => {
  const [isPlaying, setIsPlaying] = useState(false);
  const [currentTime, setCurrentTime] = useState(0);
  const [duration, setDuration] = useState(0);
  const [error, setError] = useState<string | null>(null);
  const [isReady, setIsReady] = useState(false);
  const audioRef = useRef<AudioContext | null>(null);
  const sourceRef = useRef<AudioBufferSourceNode | null>(null);
  const startTimeRef = useRef<number>(0);
  const audioBufferRef = useRef<AudioBuffer | null>(null);
  const rafRef = useRef<number>();
  const seekTimeRef = useRef<number>(0);
  // Mirrors `isReady` for synchronous reads inside play()/seek(): those can
  // run in the same tick as the state flip that makes them ready (e.g. right
  // after decoding finishes), before React has re-rendered with the new
  // `isReady` value.
  const readyRef = useRef(false);
  // A seek or play request made while the buffer is still decoding. Both are
  // per-recording: cleared whenever audioPath changes, loading fails, or the
  // hook unmounts.
  const pendingSeekRef = useRef<number | null>(null);
  const pendingPlayRef = useRef(false);

  const markNotReady = () => {
    readyRef.current = false;
    setIsReady(false);
    pendingSeekRef.current = null;
    pendingPlayRef.current = false;
  };

  const initAudioContext = async () => {
    try {
      if (!audioRef.current) {
        console.log('Creating new AudioContext');
        const AudioContextClass = window.AudioContext || (window as any).webkitAudioContext;
        audioRef.current = new AudioContextClass();
        console.log('AudioContext created:', {
          state: audioRef.current.state,
          sampleRate: audioRef.current.sampleRate,
        });
      }

      if (audioRef.current.state === 'suspended') {
        console.log('Resuming suspended AudioContext');
        await audioRef.current.resume();
        console.log('AudioContext resumed:', audioRef.current.state);
      }

      setError(null);
      return true;
    } catch (error) {
      console.error('Error initializing AudioContext:', error);
      setError('Failed to initialize audio');
      return false;
    }
  };

  // Cleanup function
  useEffect(() => {
    return () => {
      console.log('Cleaning up audio resources');
      pendingSeekRef.current = null;
      pendingPlayRef.current = false;
      if (rafRef.current) {
        cancelAnimationFrame(rafRef.current);
      }
      if (sourceRef.current) {
        sourceRef.current.stop();
      }
      if (audioRef.current) {
        audioRef.current.close();
      }
    };
  }, []);

  const loadAudio = async () => {
    if (!audioPath) {
      console.log('No audio path provided');
      return;
    }

    try {
      // Initialize context first
      const initialized = await initAudioContext();
      if (!initialized || !audioRef.current) {
        console.error('Failed to initialize audio context');
        return;
      }

      console.log('Loading audio from:', audioPath);

      // Read the file using Tauri command
      const result = await invoke<number[]>('read_audio_file', {
        filePath: audioPath
      });

      if (!result || result.length === 0) {
        throw new Error('Empty audio data received');
      }

      console.log('Audio file read, size:', result.length, 'bytes');

      // Create a copy of the audio data
      const audioData = new Uint8Array(result).buffer;

      console.log('Created audio buffer, size:', audioData.byteLength, 'bytes');

      // Decode the audio data
      const audioBuffer = await new Promise<AudioBuffer>((resolve, reject) => {
        audioRef.current!.decodeAudioData(
          audioData,
          buffer => {
            console.log('Audio decoded successfully:', {
              duration: buffer.duration,
              sampleRate: buffer.sampleRate,
              numberOfChannels: buffer.numberOfChannels,
              length: buffer.length
            });
            resolve(buffer);
          },
          error => {
            console.error('Audio decoding failed:', error);
            reject(new Error('Failed to decode audio data: ' + error));
          }
        );
      });

      audioBufferRef.current = audioBuffer;
      setDuration(audioBuffer.duration);

      // A seek requested while this was decoding wins over the default 0 -
      // that's the position the person actually asked for.
      const pendingSeek = pendingSeekRef.current;
      pendingSeekRef.current = null;
      const startAt = pendingSeek !== null ? clampSeekTime(pendingSeek, audioBuffer.duration) : 0;
      seekTimeRef.current = startAt;
      setCurrentTime(startAt);

      setError(null);
      readyRef.current = true;
      setIsReady(true);
      console.log('Audio loaded and ready to play');

      if (pendingPlayRef.current) {
        pendingPlayRef.current = false;
        console.log('Starting playback that was requested before decoding finished');
        await play();
      }
    } catch (error) {
      console.error('Error loading audio:', error);
      if (error instanceof Error) {
        console.error('Error details:', {
          message: error.message,
          name: error.name,
          stack: error.stack,
        });
      }
      markNotReady();
      setError('Failed to load audio file');
    }
  };

  // Load audio when path changes
  useEffect(() => {
    console.log('Audio path changed:', audioPath);
    // A new recording starts unseeked, with its own ready/pending state -
    // none of that should carry over from whatever was loaded before.
    markNotReady();
    audioBufferRef.current = null;
    seekTimeRef.current = 0;
    setCurrentTime(0);
    setDuration(0);
    if (audioPath) {
      loadAudio();
    }
  }, [audioPath]);

  const stopPlayback = () => {
    console.log('Stopping playback');
    if (rafRef.current) {
      cancelAnimationFrame(rafRef.current);
      rafRef.current = undefined;
    }
    if (sourceRef.current) {
      try {
        sourceRef.current.stop();
        sourceRef.current.disconnect();
      } catch (e) {
        console.log('Error stopping source:', e);
      }
      sourceRef.current = null;
    }
    setIsPlaying(false);
  };

  const play = async () => {
    console.log('Play requested');

    if (!readyRef.current || !audioBufferRef.current) {
      // Still decoding: remember the intent instead of failing. loadAudio()
      // replays it once the buffer is in hand.
      console.log('Play requested before audio is ready; remembering it');
      pendingPlayRef.current = true;
      return;
    }

    try {
      // Initialize context if needed
      const initialized = await initAudioContext();
      if (!initialized) {
        throw new Error('Audio context initialization failed');
      }
      if (!audioRef.current) {
        throw new Error('Audio context is null after initialization');
      }
      if (!audioBufferRef.current) {
        throw new Error('No audio buffer loaded - try loading the audio file first');
      }
      if (audioRef.current.state !== 'running') {
        throw new Error(`Audio context is in invalid state: ${audioRef.current.state}`);
      }

      // Stop any existing playback
      stopPlayback();

      // Create and setup new source
      console.log('Creating new audio source');
      sourceRef.current = audioRef.current.createBufferSource();
      sourceRef.current.buffer = audioBufferRef.current;

      console.log('Audio buffer details:', {
        duration: audioBufferRef.current.duration,
        sampleRate: audioBufferRef.current.sampleRate,
        numberOfChannels: audioBufferRef.current.numberOfChannels,
        length: audioBufferRef.current.length
      });

      sourceRef.current.connect(audioRef.current.destination);

      // Setup ended callback
      sourceRef.current.onended = () => {
        console.log('Playback ended naturally');
        stopPlayback();
        setCurrentTime(0);
      };

      // Start playback from the seek time
      const startTime = seekTimeRef.current;
      startTimeRef.current = audioRef.current.currentTime - startTime;
      console.log('Starting playback', {
        startTime,
        contextTime: audioRef.current.currentTime,
        seekTime: seekTimeRef.current
      });

      sourceRef.current.start(0, startTime);
      setIsPlaying(true);
      setError(null);

      // Setup time update
      const updateTime = () => {
        if (!audioRef.current || !sourceRef.current) {
          console.log('Update cancelled - context or source is null');
          return;
        }

        const newTime = audioRef.current.currentTime - startTimeRef.current;

        if (newTime >= duration) {
          console.log('Playback finished');
          stopPlayback();
          setCurrentTime(0);
          seekTimeRef.current = 0;
        } else {
          setCurrentTime(newTime);
          seekTimeRef.current = newTime;
          rafRef.current = requestAnimationFrame(updateTime);
        }
      };

      rafRef.current = requestAnimationFrame(updateTime);
    } catch (error) {
      console.error('Error during playback:', error);
      setError('Failed to play audio');
      stopPlayback();
    }
  };

  const seek = async (time: number) => {
    const clamped = clampSeekTime(time, duration);
    console.log('Seek requested:', time, '-> clamped:', clamped);

    if (!readyRef.current) {
      // Nothing decoded yet to seek within. Remember the request and move
      // the visible playhead now, so the scrubber and the highlighted
      // transcript line respond immediately instead of waiting on decoding.
      pendingSeekRef.current = clamped;
      seekTimeRef.current = clamped;
      setCurrentTime(clamped);
      return;
    }

    const wasPlaying = isPlaying;

    // Stop current playback
    stopPlayback();

    // Update both current time and seek time reference
    seekTimeRef.current = clamped;
    setCurrentTime(clamped);

    // If it was playing before, restart playback at new position
    if (wasPlaying) {
      console.log('Restarting playback at:', clamped);
      await play();
    }
  };

  const pause = () => {
    console.log('Pause requested');
    // A play queued while decoding shouldn't surprise the person by starting
    // after they've pressed pause.
    pendingPlayRef.current = false;
    stopPlayback();
  };

  return {
    isPlaying,
    currentTime,
    duration,
    error,
    isReady,
    play,
    pause,
    seek
  };
};
