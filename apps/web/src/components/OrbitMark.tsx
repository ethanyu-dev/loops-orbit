/** 品牌图形使用可缩放矢量，所有页面共享同一个视觉标识。 */
export function OrbitMark({ small = false }: { small?: boolean }) {
  return (
    <svg
      className={small ? 'orbit-mark small' : 'orbit-mark'}
      viewBox="0 0 48 48"
      fill="none"
      aria-hidden="true"
    >
      <ellipse
        cx="24"
        cy="24"
        rx="12"
        ry="20"
        transform="rotate(42 24 24)"
        stroke="currentColor"
        strokeWidth="2.4"
      />
      <path d="M10 37 38 10" stroke="currentColor" strokeWidth="2.4" />
      <circle cx="34" cy="12" r="4" fill="currentColor" />
    </svg>
  );
}
