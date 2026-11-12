"use client";
import { motion } from "framer-motion";

// Fades and lifts once when scrolled into view. Used on the few blocks that deserve an entrance.
export default function Reveal({ children, className = "", delay = 0 }: { children: React.ReactNode; className?: string; delay?: number }) {
  return (
    <motion.div className={className} initial={{ opacity: 0, y: 36 }} whileInView={{ opacity: 1, y: 0 }} viewport={{ once: true, margin: "-12% 0px" }} transition={{ duration: 1, delay, ease: [0.22, 1, 0.36, 1] }}>
      {children}
    </motion.div>
  );
}
