# Interactive root bash. Sourced by /etc/profile for login shells and by bash
# itself for interactive non-login shells.

# The console is a plain 16550A serial line, not a windowed terminal: readline's
# bracketed-paste wrapper only sprays \e[?2004h/l around every prompt and
# command here, so turn it off and keep the prompt on a line of its own.
bind 'set enable-bracketed-paste off' 2>/dev/null

case "${TERM:-dumb}" in
    dumb) PS1='\u@\H:\w \$ ' ;;
    *) PS1='\u@\H:\[\e[34m\]\w\[\e[0m\] \$ ' ;;
esac
export PATH=/usr/local/bin:/usr/local/sbin:/bin:/sbin:/usr/bin:/usr/sbin
alias ll='ls -alF'
