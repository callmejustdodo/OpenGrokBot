// SC-015 composer: ⌘↩ sends; the text stays until formationd accepts the question.
import { useState } from "react";
import { useTranslation } from "react-i18next";

export type ComposerProps = {
  disabled: boolean;
  pending: boolean;
  placeholder: string;
  /** Resolves `true` when the question was accepted. */
  onSend: (text: string) => Promise<boolean>;
};

export default function Composer({ disabled, pending, placeholder, onSend }: ComposerProps) {
  const { t } = useTranslation();
  const [text, setText] = useState("");
  const ready = !disabled && !pending && text.trim() !== "";

  const send = () => {
    if (!ready) return;
    void onSend(text.trim()).then((sent) => {
      if (sent) setText("");
    });
  };

  return (
    <div className={disabled ? "composer off" : "composer"} data-state={pending ? "pending" : undefined}>
      <textarea
        aria-label={t("ask.questionInput", "Question")}
        placeholder={placeholder}
        value={text}
        disabled={disabled}
        onChange={(e) => setText(e.target.value)}
        onKeyDown={(e) => {
          if (e.key === "Enter" && e.metaKey) {
            e.preventDefault();
            send();
          }
        }}
      />
      <button
        type="button"
        className="cbtn go"
        aria-label={t("ask.send", "Send ⌘↩")}
        aria-keyshortcuts="Meta+Enter"
        disabled={!ready}
        onClick={send}
      />
    </div>
  );
}
