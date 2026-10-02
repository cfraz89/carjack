/* fdk-aac includes this on Android for one security-event log call. */
#pragma once
#define android_errorWriteLog(tag, subTag) ((void)0)
