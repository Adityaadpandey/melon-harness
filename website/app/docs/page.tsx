import { redirect } from "next/navigation";
import { pages } from "@/lib/docs";

export default function DocsIndex() {
  redirect(`/docs/${pages[0].slug}`);
}
