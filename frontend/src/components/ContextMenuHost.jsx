import React, { useCallback, useEffect, useLayoutEffect, useRef, useState } from 'react';
import { Check, ChevronRight } from 'lucide-react';
import { editMenuItems, registerContextMenuHost, showContextMenu } from '../lib/contextMenu';

const EDGE = 6;

function shortcutLabel(accelerator) {
  return accelerator.replace(/CommandOrControl|CmdOrCtrl/g, 'Ctrl');
}

// One level of an in-page menu. Submenus render as siblings so that the
// panel's entry animation (a transform) does not offset their fixed position.
function MenuPanel({ items, x, y, flipX = null, onChoose }) {
  const panelRef = useRef(null);
  const [open, setOpen] = useState(null);
  const showsChecks = items.some((item) => typeof item.checked === 'boolean');

  // Keeps the panel inside the window before it is painted.
  useLayoutEffect(() => {
    const panel = panelRef.current;
    const { width, height } = panel.getBoundingClientRect();
    let left = x;
    let top = y;
    if (left + width > window.innerWidth - EDGE) {
      left = flipX === null ? window.innerWidth - width - EDGE : flipX - width;
    }
    if (top + height > window.innerHeight - EDGE) {
      top = window.innerHeight - height - EDGE;
    }
    panel.style.left = `${Math.max(EDGE, left)}px`;
    panel.style.top = `${Math.max(EDGE, top)}px`;
  }, [x, y, flipX]);

  const hover = (event, item, index) => {
    if (!item.submenu?.length || item.enabled === false) {
      setOpen(null);
      return;
    }
    const bounds = event.currentTarget.getBoundingClientRect();
    setOpen({ index, x: bounds.right - 2, y: bounds.top - 5, flipX: bounds.left + 2 });
  };

  const choose = (event, item, index) => {
    if (item.enabled === false) return;
    if (item.submenu) {
      hover(event, item, index);
      return;
    }
    onChoose(item);
  };

  const submenu = open ? items[open.index]?.submenu : null;

  return (
    <>
      <div
        ref={panelRef}
        className="context-menu"
        role="menu"
        style={{ left: x, top: y }}
      >
        {items.map((item, index) => {
          if (item.type === 'separator') {
            return <div key={`separator-${index}`} className="context-menu-separator" role="separator" />;
          }
          const disabled = item.enabled === false;
          return (
            <button
              key={`${item.label}-${index}`}
              type="button"
              tabIndex={-1}
              role={typeof item.checked === 'boolean' ? 'menuitemcheckbox' : 'menuitem'}
              aria-checked={typeof item.checked === 'boolean' ? item.checked : undefined}
              aria-disabled={disabled || undefined}
              aria-haspopup={item.submenu ? 'menu' : undefined}
              aria-expanded={item.submenu ? open?.index === index : undefined}
              className={[
                'context-menu-item',
                item.danger ? 'danger' : '',
                open?.index === index ? 'context-menu-item--open' : '',
              ].filter(Boolean).join(' ')}
              onMouseEnter={(event) => hover(event, item, index)}
              onClick={(event) => choose(event, item, index)}
            >
              {showsChecks && (
                <span className="context-menu-check">
                  {item.checked ? <Check size={13} strokeWidth={2.4} /> : null}
                </span>
              )}
              <span className="context-menu-label-text">{item.label}</span>
              {item.accelerator && !item.submenu && (
                <span className="context-menu-shortcut">{shortcutLabel(item.accelerator)}</span>
              )}
              {item.submenu && <ChevronRight size={13} className="context-menu-chevron" />}
            </button>
          );
        })}
      </div>
      {submenu?.length > 0 && (
        <MenuPanel
          key={open.index}
          items={submenu}
          x={open.x}
          y={open.y}
          flipX={open.flipX}
          onChoose={onChoose}
        />
      )}
    </>
  );
}

// Renders in-page menus requested through showContextMenu and offers the
// edit menu wherever no component claimed the right-click.
export default function ContextMenuHost() {
  const [menu, setMenu] = useState(null);
  const layerRef = useRef(null);
  const close = useCallback(() => setMenu(null), []);

  useEffect(() => {
    let opened = 0;
    return registerContextMenuHost((next) => {
      opened += 1;
      setMenu({ ...next, id: opened });
    });
  }, []);

  useEffect(() => {
    const onContextMenu = (event) => {
      if (event.defaultPrevented) return;
      const items = editMenuItems(event.target, window.getSelection()?.toString() || '');
      if (items.length) showContextMenu(event, items);
    };
    window.addEventListener('contextmenu', onContextMenu);
    return () => window.removeEventListener('contextmenu', onContextMenu);
  }, []);

  useEffect(() => {
    if (!menu) return undefined;
    const outside = (event) => {
      if (!layerRef.current?.contains(event.target)) close();
    };
    const onKeyDown = (event) => {
      if (event.key === 'Escape') {
        event.preventDefault();
        close();
      }
    };
    window.addEventListener('pointerdown', outside, true);
    window.addEventListener('wheel', outside, { capture: true, passive: true });
    window.addEventListener('keydown', onKeyDown, true);
    window.addEventListener('blur', close);
    window.addEventListener('resize', close);
    return () => {
      window.removeEventListener('pointerdown', outside, true);
      window.removeEventListener('wheel', outside, { capture: true });
      window.removeEventListener('keydown', onKeyDown, true);
      window.removeEventListener('blur', close);
      window.removeEventListener('resize', close);
    };
  }, [menu, close]);

  const choose = useCallback((item) => {
    setMenu(null);
    // Runs after the menu is gone, so confirmations and dialogs open cleanly.
    window.setTimeout(() => item.onSelect?.(), 0);
  }, []);

  if (!menu) return null;
  return (
    <div
      ref={layerRef}
      className="context-menu-layer"
      // Keeps focus and the text selection where the right-click happened.
      onMouseDown={(event) => event.preventDefault()}
      onContextMenu={(event) => event.preventDefault()}
    >
      <MenuPanel key={menu.id} items={menu.items} x={menu.x} y={menu.y} onChoose={choose} />
    </div>
  );
}
