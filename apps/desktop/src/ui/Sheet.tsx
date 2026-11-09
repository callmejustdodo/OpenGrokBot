// A sheet over the main window (Radix Dialog): focus is trapped and restored,
// and Esc closes it unless `onEscape` decides otherwise (the approval sheet
// asks first). Styles are D's.
import * as Dialog from "@radix-ui/react-dialog";
import type { ReactNode } from "react";

export type SheetProps = {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  title: ReactNode;
  /** A line under the title. */
  lead?: ReactNode;
  children?: ReactNode;
  footer?: ReactNode;
  /** Called on Esc; call `preventDefault()` to keep the sheet open. */
  onEscape?: (event: KeyboardEvent) => void;
};

export default function Sheet({ open, onOpenChange, title, lead, children, footer, onEscape }: SheetProps) {
  return (
    <Dialog.Root open={open} onOpenChange={onOpenChange}>
      <Dialog.Portal>
        <Dialog.Overlay className="scrim" />
        <Dialog.Content className="sheet" onEscapeKeyDown={onEscape} aria-describedby={undefined}>
          <Dialog.Title>{title}</Dialog.Title>
          {lead ? <p className="lead">{lead}</p> : null}
          {children}
          {footer ? <div className="sheet-foot">{footer}</div> : null}
        </Dialog.Content>
      </Dialog.Portal>
    </Dialog.Root>
  );
}
