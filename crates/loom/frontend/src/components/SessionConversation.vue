<script setup lang="ts">
import { computed } from 'vue';
import type { Session, AcpCommand } from '../types';
import TerminalConversation from './TerminalConversation.vue';
import AcpConversation from './AcpConversation.vue';

// The Conversation surface picks its data source by the session's execution
// backend. An ACP session (`protocol='acp'`) renders from the live chat journal
// (`/chat` + `/chat/stream`); a terminal session keeps the iris scrape path
// (`/conversation`) untouched. One prop, one seam — everything backend-specific
// lives in the two child components.
const props = withDefaults(defineProps<{ session: Session; localCommands?: AcpCommand[] }>(), {
  localCommands: () => [],
});
const emit = defineEmits<{ command: [name: string, args: string] }>();
const isAcp = computed(() => props.session.protocol === 'acp');
const forwardCommand = (name: string, args: string) => emit('command', name, args);
</script>

<template>
  <AcpConversation
    v-if="isAcp"
    :session="session"
    :local-commands="localCommands"
    @command="forwardCommand"
  />
  <TerminalConversation v-else :session="session" />
</template>
