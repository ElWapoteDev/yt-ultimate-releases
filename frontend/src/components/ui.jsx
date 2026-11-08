import React from 'react';
import { Music, Play, Pause } from 'lucide-react';
import { thumbUrl } from '../lib/format';

// Square/round thumbnail with icon fallback. size: xs | sm | md | lg | xl | square
export function Thumb({ src, size = 'sm', icon, iconSize = 14, children }) {
  const Icon = icon || Music;
  return (
    <div className={`thumb thumb--${size}`}>
      {src ? (
        <img src={src} alt="" loading="lazy" draggable={false} />
      ) : (
        <div className="thumb-placeholder"><Icon size={iconSize} /></div>
      )}
      {children}
    </div>
  );
}

export function EmptyState({ icon: Icon, title, hint, action, compact }) {
  return (
    <div className={`empty-state ${compact ? 'empty-state--compact' : ''}`}>
      {Icon && <Icon className="empty-state__icon" size={compact ? 24 : 30} strokeWidth={1.4} />}
      {title && <span className="empty-state__title">{title}</span>}
      {hint && <span className="empty-state__hint">{hint}</span>}
      {action}
    </div>
  );
}

export function PlayingIndicator() {
  return (
    <span className="eq" aria-hidden="true">
      <span /><span /><span />
    </span>
  );
}

// Grid card with hover play overlay
export function MediaCard({
  file, title, subtitle, isCurrent, isPlaying,
  onClick, onContextMenu, draggable, onDragStart, icon,
}) {
  const Icon = icon || Music;
  const src = thumbUrl(file);
  return (
    <button
      className="media-card"
      onClick={onClick}
      onContextMenu={onContextMenu}
      draggable={draggable}
      onDragStart={onDragStart}
      title={title}
    >
      <div className="card-cover">
        <div className="thumb thumb--square">
          {src ? (
            <img src={src} alt="" loading="lazy" draggable={false} />
          ) : (
            <div className="thumb-placeholder"><Icon size={26} strokeWidth={1.5} /></div>
          )}
        </div>
        <span className="card-play">
          {isCurrent && isPlaying
            ? <Pause size={15} fill="currentColor" />
            : <Play size={15} fill="currentColor" style={{ marginLeft: 2 }} />}
        </span>
        {isCurrent && isPlaying && (
          <span className="card-playing"><PlayingIndicator /></span>
        )}
      </div>
      <div className={`media-card__title ${isCurrent ? 'media-card__title--active' : ''}`}>{title}</div>
      {subtitle ? <div className="media-card__sub">{subtitle}</div> : null}
    </button>
  );
}
