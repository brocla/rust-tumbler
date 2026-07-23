import { useEffect, useState } from "react";

const GLYPHS = ["🎵", "🎶", "♪", "♫", "♬"];
const NOTE_COUNT = 28;
// Roughly this share of notes go red. Only the text glyphs (♪♫♬) actually
// take the color — the emoji ones (🎵🎶) paint themselves — which mixes the
// palette nicely.
const RED_SHARE = 0.4;
const RED = "#d3212d";
// Longest launch delay plus rise time; the overlay unmounts itself after this
// so no invisible nodes linger for the rest of the greeting's 30 seconds.
const BURST_MS = 9000;

interface Note {
  id: number;
  glyph: string;
  left: number; // % of viewport width
  size: number; // px
  delay: number; // s
  duration: number; // s
  drift: number; // px of sideways wander over the rise
  spin: number; // deg
  color?: string; // overrides the accent color (some notes go red)
}

function makeNotes(): Note[] {
  return Array.from({ length: NOTE_COUNT }, (_, i) => ({
    id: i,
    glyph: GLYPHS[i % GLYPHS.length],
    left: 4 + Math.random() * 92,
    size: 28 + Math.random() * 24,
    delay: Math.random() * 1.8,
    duration: 3.5 + Math.random() * 3,
    drift: (Math.random() - 0.5) * 120,
    spin: (Math.random() - 0.5) * 60,
    color: Math.random() < RED_SHARE ? RED : undefined,
  }));
}

/**
 * A fire-and-forget burst of musical notes rising from the status bar — part
 * of the Margins panel's birthday easter egg (the tool was a gift; see
 * MarginsPanel's `celebrate`). Purely decorative: pointer-transparent, hidden
 * from the accessibility tree, suppressed under prefers-reduced-motion (in
 * global.css), and it removes itself once the last note has floated away.
 */
export function BirthdayNotes() {
  const [notes] = useState(makeNotes);
  const [done, setDone] = useState(false);

  useEffect(() => {
    const t = window.setTimeout(() => setDone(true), BURST_MS);
    return () => window.clearTimeout(t);
  }, []);

  if (done) return null;
  return (
    <div className="birthday-notes" aria-hidden="true">
      {notes.map((n) => (
        <span
          key={n.id}
          className="birthday-note"
          style={
            {
              left: `${n.left}%`,
              fontSize: `${n.size}px`,
              ...(n.color ? { color: n.color } : {}),
              animationDelay: `${n.delay}s`,
              animationDuration: `${n.duration}s`,
              "--drift": `${n.drift}px`,
              "--spin": `${n.spin}deg`,
            } as React.CSSProperties
          }
        >
          {n.glyph}
        </span>
      ))}
    </div>
  );
}
