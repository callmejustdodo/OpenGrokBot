// A confirmation alert (Radix AlertDialog), for choices such as "Stop now" or
// "Deny". Actions are buttons the caller passes; none is bound to Return.
import * as AlertDialog from "@radix-ui/react-alert-dialog";
import type { ReactNode } from "react";

export type AlertProps = {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  title: ReactNode;
  description?: ReactNode;
  /** The buttons, stacked; wrap the dismissing one in `AlertCancel`. */
  children: ReactNode;
};

export default function Alert({ open, onOpenChange, title, description, children }: AlertProps) {
  return (
    <AlertDialog.Root open={open} onOpenChange={onOpenChange}>
      <AlertDialog.Portal>
        <AlertDialog.Overlay className="scrim" />
        <AlertDialog.Content className="alert">
          <AlertDialog.Title>{title}</AlertDialog.Title>
          {description ? <AlertDialog.Description>{description}</AlertDialog.Description> : null}
          <div className="stack">{children}</div>
        </AlertDialog.Content>
      </AlertDialog.Portal>
    </AlertDialog.Root>
  );
}

export const AlertCancel = AlertDialog.Cancel;
export const AlertAction = AlertDialog.Action;
