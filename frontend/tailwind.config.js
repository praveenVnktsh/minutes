/** @type {import('tailwindcss').Config} */
module.exports = {
    darkMode: ['class'],
    content: [
    './src/pages/**/*.{js,ts,jsx,tsx,mdx}',
    './src/components/**/*.{js,ts,jsx,tsx,mdx}',
    './src/app/**/*.{js,ts,jsx,tsx,mdx}',
  ],
  theme: {
  	extend: {
  		fontFamily: {
  			sans: [
  				'var(--font-source-sans-3)'
  			],
  			serif: [
  				'var(--font-source-serif-4)'
  			]
  		},
  		colors: {
  			background: 'hsl(var(--background))',
  			foreground: 'hsl(var(--foreground))',
  			surface: {
  				'0': 'var(--surface-0)',
  				'1': 'var(--surface-1)',
  				'2': 'var(--surface-2)',
  				'raised': 'var(--surface-raised)'
  			},
  			ink: {
  				DEFAULT: 'var(--ink)',
  				muted: 'var(--ink-muted)',
  				subtle: 'var(--ink-subtle)'
  			},
  			hairline: 'var(--hairline)',
			brand: {
  				DEFAULT: 'var(--brand)',
  				foreground: 'var(--brand-foreground)'
			},
			selected: {
				DEFAULT: 'rgb(var(--selected-rgb) / <alpha-value>)',
				foreground: 'rgb(var(--selected-foreground-rgb) / <alpha-value>)'
			},
			focus: 'rgb(var(--focus-rgb) / <alpha-value>)',
			info: {
				DEFAULT: 'rgb(var(--info-rgb) / <alpha-value>)',
				subtle: 'rgb(var(--info-subtle-rgb) / <alpha-value>)'
			},
			success: {
				DEFAULT: 'rgb(var(--success-rgb) / <alpha-value>)',
				subtle: 'rgb(var(--success-subtle-rgb) / <alpha-value>)'
			},
			warning: {
				DEFAULT: 'rgb(var(--warning-rgb) / <alpha-value>)',
				subtle: 'rgb(var(--warning-subtle-rgb) / <alpha-value>)'
			},
			error: {
				DEFAULT: 'rgb(var(--error-rgb) / <alpha-value>)',
				subtle: 'rgb(var(--error-subtle-rgb) / <alpha-value>)'
			},
			recording: {
				DEFAULT: 'rgb(var(--recording-rgb) / <alpha-value>)',
				subtle: 'rgb(var(--recording-subtle-rgb) / <alpha-value>)'
			},
			paused: {
				DEFAULT: 'rgb(var(--paused-rgb) / <alpha-value>)',
				subtle: 'rgb(var(--paused-subtle-rgb) / <alpha-value>)'
			},
  			border: 'hsl(var(--border))',
  			input: 'hsl(var(--input))',
  			ring: 'hsl(var(--ring))',
  			primary: {
  				DEFAULT: 'hsl(var(--primary))',
  				foreground: 'hsl(var(--primary-foreground))'
  			},
  			secondary: {
  				DEFAULT: 'hsl(var(--secondary))',
  				foreground: 'hsl(var(--secondary-foreground))'
  			},
  			tertiary: '#64748b',
  			card: {
  				DEFAULT: 'hsl(var(--card))',
  				foreground: 'hsl(var(--card-foreground))'
  			},
  			popover: {
  				DEFAULT: 'hsl(var(--popover))',
  				foreground: 'hsl(var(--popover-foreground))'
  			},
  			muted: {
  				DEFAULT: 'hsl(var(--muted))',
  				foreground: 'hsl(var(--muted-foreground))'
  			},
  			accent: {
  				DEFAULT: 'hsl(var(--accent))',
  				foreground: 'hsl(var(--accent-foreground))'
  			},
  			destructive: {
  				DEFAULT: 'hsl(var(--destructive))',
  				foreground: 'hsl(var(--destructive-foreground))'
  			},
  			chart: {
  				'1': 'hsl(var(--chart-1))',
  				'2': 'hsl(var(--chart-2))',
  				'3': 'hsl(var(--chart-3))',
  				'4': 'hsl(var(--chart-4))',
  				'5': 'hsl(var(--chart-5))'
  			}
  		},
  		borderRadius: {
  			lg: 'var(--radius)',
  			md: 'calc(var(--radius) - 2px)',
  			sm: 'calc(var(--radius) - 4px)'
  		},
  		keyframes: {
  			'accordion-down': {
  				from: {
  					height: '0'
  				},
  				to: {
  					height: 'var(--radix-accordion-content-height)'
  				}
  			},
  			'accordion-up': {
  				from: {
  					height: 'var(--radix-accordion-content-height)'
  				},
  				to: {
  					height: '0'
  				}
  			}
  		},
  		animation: {
  			'accordion-down': 'accordion-down 0.2s ease-out',
  			'accordion-up': 'accordion-up 0.2s ease-out'
  		}
  	}
  },
  plugins: [
    require("tailwindcss-animate"),
    require("@tailwindcss/typography"),
    require("@tailwindcss/container-queries"),
  ],
}
