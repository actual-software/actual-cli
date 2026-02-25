import React from "react";
import { COLORS, FONTS } from "../../data/brand";

const BANNER_LINES = [
  "       ..===************",
  "     ..****************",
  "   .******      ********+************",
  "  =*****   ****   +*****************",
  "*******   ******   .*** :****",
  "  ****=   ******   **********+++=",
  "   *****    **.    **+ :**+",
  "    *****.         ************",
  "      ***********- *********-",
  "        **********************+",
  "           :-++**********",
];

function interpolateColor(t: number): string {
  // t: 0 = #00FB7E, 1 = #179CA9
  const r = Math.round(0x00 + t * (0x17 - 0x00));
  const g = Math.round(0xfb + t * (0x9c - 0xfb));
  const b = Math.round(0x7e + t * (0xa9 - 0x7e));
  return `rgb(${r},${g},${b})`;
}

export const LogoPanel: React.FC = () => {
  return (
    <div
      style={{
        fontFamily: FONTS.mono,
        fontSize: 12,
        lineHeight: 1.4,
        padding: "8px 12px",
        color: COLORS.textPrimary,
      }}
    >
      {BANNER_LINES.map((line, lineIdx) => (
        <div key={lineIdx} style={{ whiteSpace: "pre" }}>
          {line.split("").map((char, charIdx) => {
            const maxWidth = 44; // left panel character width
            const t = charIdx / maxWidth;
            return (
              <span key={charIdx} style={{ color: interpolateColor(t) }}>
                {char}
              </span>
            );
          })}
        </div>
      ))}
    </div>
  );
};
