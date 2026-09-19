'use client'

import { useEffect } from 'react'
import { MeetingsLibrary } from '@/components/MeetingsLibrary'
import Analytics from '@/lib/analytics'

export default function MeetingsPage() {
  useEffect(() => {
    void Analytics.trackPageView('meetings')
  }, [])
  return <MeetingsLibrary />
}
