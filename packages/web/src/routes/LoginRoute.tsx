
import { useTranslation } from "../i18n";
import { useNavigate, useSearchParams } from "react-router";
import { LoginForm } from "../components/LoginForm";
import { OnboardingUtilities } from "../components/OnboardingUtilities";

function safeReturnTo(value: string | null) {
  if (!value) return "/";
  try {
    const url = new URL(value, window.location.origin);
    return url.origin === window.location.origin ? `${url.pathname}${url.search}${url.hash}` : "/";
  } catch {
    return "/";
  }
}

export default function LoginPage() {
  const { t } = useTranslation();
  const navigate = useNavigate();
  const [searchParams] = useSearchParams();

  return (
    <main className="login">
      <LoginForm
        initialEmail={searchParams.get("email") || ""}
        notice={searchParams.get("reset") === "1" ? t("loginRoute.yourPasswordHasBeenUpdatedSignInWithYour") : ""}
        onAuthenticated={() => navigate(safeReturnTo(searchParams.get("next")), { replace: true })}
      />
      <OnboardingUtilities />
    </main>
  );
}
